use std::collections::VecDeque;

pub const MAX_ENTRIES: usize = 300;
pub const MAX_ENTRY_BYTES: usize = 4096;

#[derive(Default)]
pub struct History(VecDeque<String>);
impl History {
    pub fn push(&mut self, mut line: String) {
        if line.len() > MAX_ENTRY_BYTES {
            let mut end = MAX_ENTRY_BYTES - '…'.len_utf8();
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
            line.push('…');
        }
        if line.capacity() > MAX_ENTRY_BYTES {
            line.shrink_to_fit();
        }
        self.0.push_back(line);
        while self.0.len() > MAX_ENTRIES {
            self.0.pop_front();
        }
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &String> {
        self.0.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn copy_text(&self) -> String {
        self.0.iter().rev().cloned().collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_only_the_newest_300_entries_and_copies_in_display_order() {
        let mut history = History::default();
        for index in 0..305 {
            history.push(index.to_string());
        }
        assert_eq!(history.iter().count(), 300);
        assert_eq!(history.iter().next().unwrap(), "5");
        assert!(history.copy_text().starts_with("304\n303\n"));
        assert!(history.copy_text().ends_with("\n5"));
    }
    #[test]
    fn caps_large_entries_without_breaking_unicode() {
        let mut history = History::default();
        history.push("🥣".repeat(5000));
        let line = history.iter().next().unwrap();
        assert!(line.len() <= MAX_ENTRY_BYTES);
        assert!(line.capacity() <= MAX_ENTRY_BYTES);
        assert!(line.ends_with('…'));
        let mut reserved = String::with_capacity(1_000_000);
        reserved.push_str("tiny");
        history.push(reserved);
        assert!(history.iter().next_back().unwrap().capacity() <= MAX_ENTRY_BYTES);
        assert!(History::default().copy_text().is_empty());
    }
}
