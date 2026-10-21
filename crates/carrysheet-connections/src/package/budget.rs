//! One budget shared by every part: how many items and characters the
//! report may hold in total, and how many bytes of XML may be read in total.
//! Per-part limits alone let a package of many small parts multiply them.

use std::cell::Cell;

use crate::limits::Limits;

/// Counters that only go down. Once a charge fails, every later charge of
/// the same kind fails too, so what the report leaves out is always the tail.
#[derive(Debug)]
pub(crate) struct Budget {
    items_left: Cell<usize>,
    chars_left: Cell<usize>,
    xml_bytes_left: Cell<u64>,
    output_spent: Cell<Option<OutputLimit>>,
    reading_spent: Cell<bool>,
}

/// Which output limit ran out first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputLimit {
    Items,
    Chars,
}

impl Budget {
    pub fn new(limits: &Limits) -> Self {
        Self {
            items_left: Cell::new(limits.max_total_items),
            chars_left: Cell::new(limits.max_total_chars),
            xml_bytes_left: Cell::new(limits.max_total_xml_bytes),
            output_spent: Cell::new(None),
            reading_spent: Cell::new(false),
        }
    }

    /// Pays for one output item holding `chars` bytes of text. False when
    /// the budget cannot pay; the item must then be left out.
    pub fn output(&self, chars: usize) -> bool {
        if self.output_spent.get().is_some() {
            return false;
        }
        let items = self.items_left.get();
        let left = self.chars_left.get();
        if items == 0 {
            self.output_spent.set(Some(OutputLimit::Items));
            return false;
        }
        if left < chars {
            self.output_spent.set(Some(OutputLimit::Chars));
            return false;
        }
        self.items_left.set(items - 1);
        self.chars_left.set(left - chars);
        true
    }

    /// Bytes of XML that may still be read.
    pub fn xml_bytes_left(&self) -> u64 {
        if self.reading_spent.get() {
            0
        } else {
            self.xml_bytes_left.get()
        }
    }

    /// Pays for `bytes` of XML read. False when they were more than was
    /// left; reading stops from then on.
    pub fn read(&self, bytes: u64) -> bool {
        let left = self.xml_bytes_left();
        if bytes > left {
            self.reading_spent.set(true);
            return false;
        }
        self.xml_bytes_left.set(left - bytes);
        true
    }

    /// Marks reading as stopped.
    pub fn stop_reading(&self) {
        self.reading_spent.set(true);
    }

    pub fn output_spent(&self) -> bool {
        self.output_spent.get().is_some()
    }

    /// The output limit that ran out, if one did.
    pub fn output_hit(&self) -> Option<OutputLimit> {
        self.output_spent.get()
    }

    pub fn reading_spent(&self) -> bool {
        self.reading_spent.get()
    }
}

#[cfg(test)]
mod tests {
    use super::Budget;
    use crate::Limits;

    #[test]
    fn charges_stop_for_good() {
        let limits = Limits {
            max_total_items: 2,
            max_total_chars: 10,
            max_total_xml_bytes: 5,
            ..Limits::default()
        };
        let budget = Budget::new(&limits);
        assert!(budget.output(4));
        assert!(!budget.output(7));
        assert!(!budget.output(1), "a failed charge stops the output");
        assert!(budget.output_spent());
        assert!(budget.read(5));
        assert!(!budget.read(1));
        assert_eq!(budget.xml_bytes_left(), 0);
        assert!(budget.reading_spent());
    }
}
