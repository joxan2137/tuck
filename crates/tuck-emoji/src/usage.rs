//! Frecency: every pick adds one point to the item's score; scores halve every 14 days.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::Kind;

const HALF_LIFE_MS: f64 = 14.0 * 24.0 * 60.0 * 60.0 * 1000.0;
const FORGET_BELOW: f64 = 0.02;
const MAX_BOOST: f64 = 20.0;
const BOOST_PER_LN_POINT: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Stat {
    /// Frecency score as of `last_ms`.
    score: f64,
    last_ms: i64,
}

impl Stat {
    fn score_at(&self, now_ms: i64) -> f64 {
        let elapsed_ms = now_ms.saturating_sub(self.last_ms).max(0) as f64;
        self.score * (-elapsed_ms / HALF_LIFE_MS).exp2()
    }

    /// Orders like the score at any common instant without needing "now": `ln(score) + last / half-life · ln 2`.
    fn ranking_key(&self) -> f64 {
        self.score.ln() + self.last_ms as f64 / HALF_LIFE_MS * std::f64::consts::LN_2
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    emoji: BTreeMap<String, Stat>,
    kaomoji: BTreeMap<String, Stat>,
    symbols: BTreeMap<String, Stat>,
}

impl Usage {
    pub fn record(&mut self, kind: Kind, text: &str, now_ms: i64) {
        let stats = self.stats_mut(kind);
        stats.retain(|_, stat| stat.score_at(now_ms) >= FORGET_BELOW);
        let stat = stats.entry(text.to_string()).or_insert(Stat { score: 0.0, last_ms: now_ms });
        stat.score = stat.score_at(now_ms) + 1.0;
        stat.last_ms = stat.last_ms.max(now_ms);
    }

    /// Most frecent first; among equal scores the most recently used.
    pub fn frequent(&self, kind: Kind, limit: usize) -> Vec<String> {
        let mut ranked: Vec<(&String, &Stat)> = self.stats(kind).iter().collect();
        ranked.sort_by(|(_, a), (_, b)| b.ranking_key().total_cmp(&a.ranking_key()).then(b.last_ms.cmp(&a.last_ms)));
        ranked.into_iter().take(limit).map(|(text, _)| text.clone()).collect()
    }

    pub fn clear(&mut self, kind: Kind) {
        self.stats_mut(kind).clear();
    }

    /// Search score bonus, 0 to 20: `8 · ln(1 + score)`, so one recent pick is worth 6.
    pub(crate) fn boost(&self, kind: Kind, text: &str, now_ms: i64) -> u32 {
        self.stats(kind)
            .get(text)
            .map_or(0, |stat| (stat.score_at(now_ms).ln_1p() * BOOST_PER_LN_POINT).round().min(MAX_BOOST) as u32)
    }

    fn stats(&self, kind: Kind) -> &BTreeMap<String, Stat> {
        match kind {
            Kind::Emoji => &self.emoji,
            Kind::Kaomoji => &self.kaomoji,
            Kind::Symbol => &self.symbols,
        }
    }

    fn stats_mut(&mut self, kind: Kind) -> &mut BTreeMap<String, Stat> {
        match kind {
            Kind::Emoji => &mut self.emoji,
            Kind::Kaomoji => &mut self.kaomoji,
            Kind::Symbol => &mut self.symbols,
        }
    }
}
