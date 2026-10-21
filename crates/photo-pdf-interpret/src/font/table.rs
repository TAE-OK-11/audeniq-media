//! Static lookup tables for the generated font data (encodings, glyph names,
//! standard font metrics): entries sorted by key, found by binary search.

use core::borrow::Borrow;

pub(crate) struct Map<K: 'static, V: 'static>(&'static [(K, V)]);

impl<K: Ord, V> Map<K, V> {
    /// `entries` must be strictly sorted by key (checked by the tests below).
    pub(crate) const fn new(entries: &'static [(K, V)]) -> Self {
        Self(entries)
    }

    pub(crate) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0
            .binary_search_by(|(k, _)| k.borrow().cmp(key))
            .ok()
            .map(|i| &self.0[i].1)
    }

    #[cfg(test)]
    fn is_strictly_sorted(&self) -> bool {
        self.0.windows(2).all(|w| w[0].0 < w[1].0)
    }
}

#[cfg(test)]
mod tests {
    use crate::font::generated::*;

    macro_rules! sorted {
        ($($table:path),* $(,)?) => {
            $(assert!($table.is_strictly_sorted(), stringify!($table));)*
        };
    }

    #[test]
    fn generated_tables_are_strictly_sorted() {
        sorted!(
            glyph_names::GLYPH_NAMES,
            glyph_names::REVERSE_GLYPH_NAMES,
            mac_expert::MAC_EXPERT,
            mac_os_roman::MAC_OS_ROMAN,
            mac_os_roman::MAC_OS_ROMAN_INVERSE,
            mac_roman::MAC_ROMAN,
            mac_roman::MAC_ROMAN_INVERSE,
            standard::STANDARD,
            symbol::SYMBOL,
            win_ansi::WIN_ANSI,
            zapf_dings::ZAPF_DING_BATS,
            metrics::COURIER,
            metrics::COURIER_BOLD,
            metrics::COURIER_BOLD_OBLIQUE,
            metrics::COURIER_OBLIQUE,
            metrics::HELVETICA,
            metrics::HELVETICA_BOLD,
            metrics::HELVETICA_BOLD_OBLIQUE,
            metrics::HELVETICA_OBLIQUE,
            metrics::SYMBOL,
            metrics::TIMES_BOLD,
            metrics::TIMES_BOLD_ITALIC,
            metrics::TIMES_ITALIC,
            metrics::TIMES_ROMAN,
            metrics::ZAPF_DING_BATS,
        );
    }

    #[test]
    fn lookups_find_every_key() {
        assert_eq!(standard::get(32), Some("space"));
        assert_eq!(standard::get(0), None);
        assert_eq!(glyph_names::get("AE"), Some("\u{00C6}"));
        assert_eq!(glyph_names::get_reverse('\u{00C6}'), Some("AE"));
        assert_eq!(mac_roman::get_inverse("space"), Some(32));
        assert_eq!(metrics::COURIER.get("space"), Some(&600.0));
        assert_eq!(metrics::HELVETICA.get("no-such-glyph"), None);
    }
}
