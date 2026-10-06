use crate::image::Image;

/// Downscales `image` to fit within `max_w`×`max_h` keeping its aspect ratio (area-averaged, alpha-weighted).
/// Images already inside the box are returned unchanged.
pub fn thumbnail(image: &Image, max_w: u32, max_h: u32) -> Image {
    let (max_w, max_h) = (max_w.max(1), max_h.max(1));
    let inside_box = image.width <= max_w && image.height <= max_h;
    if inside_box || image.width == 0 || image.height == 0 {
        return image.clone();
    }
    let (width, height) = fitted_size(image.width, image.height, max_w, max_h);
    let across = spans(image.width, width);
    let down = spans(image.height, height);
    let rows = average_columns(image, &across);
    average_rows(&rows, &down, width, height)
}

/// Premultiplied BGRA with alpha on the 0..255 scale: `[b·a/255, g·a/255, r·a/255, a]`.
type Premultiplied = [f32; 4];

/// The destination pixel's source range along one axis: `first` source index and one weight per source pixel,
/// weights summing to 1.
struct Span {
    first: usize,
    weights: Vec<f32>,
}

fn fitted_size(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let width_limited = u64::from(width) * u64::from(max_h) >= u64::from(height) * u64::from(max_w);
    if width_limited { (max_w, scaled(height, max_w, width)) } else { (scaled(width, max_h, height), max_h) }
}

/// `value * numerator / denominator`, rounded, at least 1.
fn scaled(value: u32, numerator: u32, denominator: u32) -> u32 {
    let denominator = u64::from(denominator);
    let rounded = (u64::from(value) * u64::from(numerator) * 2 + denominator) / (denominator * 2);
    rounded.max(1) as u32
}

fn spans(source_len: u32, target_len: u32) -> Vec<Span> {
    let ratio = f64::from(source_len) / f64::from(target_len);
    (0..target_len)
        .map(|target| {
            let start = f64::from(target) * ratio;
            let end = f64::from(target + 1) * ratio;
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(source_len as usize);
            let weights = (first..last)
                .map(|source| {
                    let overlap = end.min(source as f64 + 1.0) - start.max(source as f64);
                    (overlap / ratio) as f32
                })
                .collect();
            Span { first, weights }
        })
        .collect()
}

fn premultiply(bgra: &[u8]) -> Premultiplied {
    let alpha = f32::from(bgra[3]);
    let scale = alpha / 255.0;
    [f32::from(bgra[0]) * scale, f32::from(bgra[1]) * scale, f32::from(bgra[2]) * scale, alpha]
}

/// Averages each source row down to `across.len()` columns.
fn average_columns(image: &Image, across: &[Span]) -> Vec<Premultiplied> {
    let mut out = Vec::with_capacity(across.len() * image.height as usize);
    for row in image.data.chunks_exact(image.stride()) {
        for span in across {
            let mut sum = [0.0; 4];
            for (offset, weight) in span.weights.iter().enumerate() {
                let start = (span.first + offset) * 4;
                let pixel = premultiply(&row[start..start + 4]);
                for (total, channel) in sum.iter_mut().zip(pixel) {
                    *total += channel * weight;
                }
            }
            out.push(sum);
        }
    }
    out
}

/// Averages the column-reduced rows down to `down.len()` rows and converts back to straight-alpha BGRA.
fn average_rows(rows: &[Premultiplied], down: &[Span], width: u32, height: u32) -> Image {
    let columns = width as usize;
    let mut out = Image::new(width, height);
    let mut accumulator = vec![[0.0f32; 4]; columns];
    for (span, out_row) in down.iter().zip(out.data.chunks_exact_mut(columns * 4)) {
        accumulator.fill([0.0; 4]);
        for (offset, weight) in span.weights.iter().enumerate() {
            let start = (span.first + offset) * columns;
            for (total, pixel) in accumulator.iter_mut().zip(&rows[start..start + columns]) {
                for (sum, channel) in total.iter_mut().zip(pixel) {
                    *sum += channel * weight;
                }
            }
        }
        for (total, out_pixel) in accumulator.iter().zip(out_row.chunks_exact_mut(4)) {
            out_pixel.copy_from_slice(&unpremultiply(total));
        }
    }
    out
}

fn unpremultiply(pixel: &Premultiplied) -> [u8; 4] {
    let alpha = pixel[3];
    if alpha <= 0.0 {
        return [0; 4];
    }
    let unscale = 255.0 / alpha;
    let byte = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    [byte(pixel[0] * unscale), byte(pixel[1] * unscale), byte(pixel[2] * unscale), byte(alpha)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(width: u32, height: u32, bgra: [u8; 4]) -> Image {
        Image::from_bgra(width, height, bgra.repeat(width as usize * height as usize))
    }

    fn from_pixels(width: u32, height: u32, pixels: &[[u8; 4]]) -> Image {
        Image::from_bgra(width, height, pixels.concat())
    }

    #[test]
    fn image_inside_the_box_is_returned_unchanged() {
        let image = from_pixels(2, 2, &[[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]]);
        assert_eq!(thumbnail(&image, 2, 2), image);
        assert_eq!(thumbnail(&image, 100, 50), image);
        assert_eq!(thumbnail(&Image::new(0, 0), 4, 4), Image::new(0, 0));
    }

    #[test]
    fn keeps_the_aspect_ratio() {
        let wide = filled(400, 200, [10, 20, 30, 255]);
        let tall = filled(200, 400, [10, 20, 30, 255]);
        let thumb = thumbnail(&wide, 100, 100);
        assert_eq!((thumb.width, thumb.height), (100, 50));
        let thumb = thumbnail(&tall, 100, 100);
        assert_eq!((thumb.width, thumb.height), (50, 100));
        let thumb = thumbnail(&wide, 200, 20);
        assert_eq!((thumb.width, thumb.height), (40, 20));
        let thumb = thumbnail(&filled(300, 200, [0; 4]), 100, 100);
        assert_eq!((thumb.width, thumb.height), (100, 67));
    }

    #[test]
    fn only_the_oversized_axis_forces_a_downscale() {
        let thumb = thumbnail(&filled(100, 10, [1, 2, 3, 255]), 50, 50);
        assert_eq!((thumb.width, thumb.height), (50, 5));
        let thumb = thumbnail(&filled(10, 100, [1, 2, 3, 255]), 50, 50);
        assert_eq!((thumb.width, thumb.height), (5, 50));
    }

    #[test]
    fn never_shrinks_an_axis_below_one_pixel() {
        let thumb = thumbnail(&filled(1000, 2, [9, 9, 9, 255]), 100, 100);
        assert_eq!((thumb.width, thumb.height), (100, 1));
        let thumb = thumbnail(&filled(1, 1000, [9, 9, 9, 255]), 100, 100);
        assert_eq!((thumb.width, thumb.height), (1, 100));
        let thumb = thumbnail(&filled(30, 20, [9, 9, 9, 255]), 0, 0);
        assert_eq!((thumb.width, thumb.height), (1, 1));
    }

    #[test]
    fn averages_whole_blocks() {
        let image = from_pixels(
            4,
            2,
            &[
                [0, 0, 0, 255],
                [100, 100, 100, 255],
                [10, 20, 30, 255],
                [30, 20, 10, 255],
                [200, 200, 200, 255],
                [100, 100, 100, 255],
                [50, 60, 70, 255],
                [10, 0, 10, 255],
            ],
        );
        let thumb = thumbnail(&image, 2, 1);
        assert_eq!((thumb.width, thumb.height), (2, 1));
        assert_eq!(thumb.pixel(0, 0), [100, 100, 100, 255]);
        assert_eq!(thumb.pixel(1, 0), [25, 25, 30, 255]);
    }

    #[test]
    fn splits_pixels_across_fractional_boundaries() {
        let mut image = Image::new(3, 3);
        for pixel in image.data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 0, 255]);
        }
        let center = (3 + 1) * 4;
        image.data[center..center + 3].copy_from_slice(&[90, 90, 90]);
        let thumb = thumbnail(&image, 2, 2);
        assert_eq!((thumb.width, thumb.height), (2, 2));
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(thumb.pixel(x, y), [10, 10, 10, 255]);
            }
        }
    }

    #[test]
    fn transparent_pixels_do_not_tint_their_neighbours() {
        let image = from_pixels(2, 1, &[[0, 0, 255, 0], [255, 255, 255, 255]]);
        let thumb = thumbnail(&image, 1, 1);
        assert_eq!(thumb.pixel(0, 0), [255, 255, 255, 128]);
    }

    #[test]
    fn partial_alpha_is_weighted_by_coverage() {
        let image = from_pixels(2, 1, &[[0, 0, 200, 255], [200, 0, 0, 85]]);
        let thumb = thumbnail(&image, 1, 1);
        let [b, g, r, a] = thumb.pixel(0, 0);
        assert_eq!(a, 170);
        assert_eq!(g, 0);
        assert_eq!(r, 150);
        assert_eq!(b, 50);
    }

    #[test]
    fn uniform_colors_survive_odd_ratios_exactly() {
        for color in [[10, 20, 30, 255], [250, 3, 77, 255], [40, 50, 60, 128]] {
            let thumb = thumbnail(&filled(37, 23, color), 7, 7);
            assert_eq!((thumb.width, thumb.height), (7, 4));
            assert!(thumb.data.chunks_exact(4).all(|pixel| pixel == color), "{color:?}");
        }
    }

    #[test]
    fn fully_transparent_stays_transparent() {
        let thumb = thumbnail(&filled(10, 10, [0, 0, 0, 0]), 3, 3);
        assert!(thumb.data.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn large_gradient_has_the_expected_size_and_stays_monotonic() {
        let (width, height) = (1000u32, 600u32);
        let mut image = Image::new(width, height);
        for (index, pixel) in image.data.chunks_exact_mut(4).enumerate() {
            let x = index as u32 % width;
            let level = (x * 255 / (width - 1)) as u8;
            pixel.copy_from_slice(&[level, level, level, 255]);
        }
        let thumb = thumbnail(&image, 100, 100);
        assert_eq!((thumb.width, thumb.height), (100, 60));
        let row: Vec<u8> = (0..thumb.width).map(|x| thumb.pixel(x, 30)[0]).collect();
        assert!(row.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(row[0] <= 3 && row[99] >= 252);
    }
}
