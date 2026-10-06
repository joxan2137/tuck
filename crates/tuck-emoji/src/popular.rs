//! Tie-break order for well-known emoji: among equal search scores the more popular one comes first, so "heart"
//! finds the red heart before the other hearts. Most popular first; entries are matched by exact text.

use std::collections::HashMap;

use crate::catalog::Entry;

const POPULAR: &str = "\
    😂 ❤️ 🤣 👍 😭 🙏 😘 🥰 😍 😊 🎉 😁 💕 🥺 😅 🔥 \
    ☺️ 🤦 ♥️ 🤷 🙄 😆 🤗 😉 🎂 🤔 👏 🙂 😳 🥳 😎 👌 \
    💜 😔 💪 ✨ 💖 👀 😋 😏 😢 👉 💗 😩 💯 🌹 💞 🎈 \
    💙 😃 😡 💐 😜 🙈 🤞 😄 🤤 🙌 🤪 ❣️ 😀 💋 💀 👇 \
    💔 😌 💓 🤩 🙃 😬 😱 😴 🤭 😐 🌞 😒 😇 🌸 😈 🎶 \
    ✌️ 🎊 🥵 😞 ✅ 😚 ☀️ 🖤 💰 😝 🙊 💘 🍀 💥 🤝 😻 \
    🤨 🙋 🥂 💛 🚀 😥 ✔️ 😤 👋 😮 🎁 🏆 😪 🤯 🥹 😑 \
    🙁 😲 🤑 🌟 ⭐ 🍕 🐶 🐱 🎵 🌈 ☕ 🍺 🍻 💩 🤡 👻 \
    🙅 🤢 😵 🥶 🧠 👑";

/// Position of each entry in `POPULAR`, `u16::MAX` for entries that are not listed.
pub(crate) fn ranks(entries: &[Entry]) -> Vec<u16> {
    let index_by_text: HashMap<&str, usize> =
        entries.iter().enumerate().map(|(index, entry)| (entry.text.as_str(), index)).collect();
    let mut ranks = vec![u16::MAX; entries.len()];
    for (rank, emoji) in POPULAR.split_whitespace().enumerate() {
        if let Some(&index) = index_by_text.get(emoji) {
            ranks[index] = rank as u16;
        }
    }
    ranks
}

#[cfg(test)]
pub(crate) fn listed() -> impl Iterator<Item = &'static str> {
    POPULAR.split_whitespace()
}
