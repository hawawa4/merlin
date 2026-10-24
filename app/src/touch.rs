//! Finding the parameter a person touched in a plugin's own window, by reading every parameter's
//! value and seeing which one moved. Plugins do not all tell the host about edits made in their
//! windows, but they all answer "where is this parameter now?".
//!
//! Three passes over the parameters: the first remembers where each one is, the second finds the
//! ones that move by themselves (an LFO, a meter, a modulated value) so they can be ignored, and
//! from the third on the first untouched-looking parameter that has moved is the one.

use std::collections::{HashMap, HashSet};

/// How far a parameter must move, as a fraction of its range, to count as touched.
const MOVED: f64 = 0.002;

pub struct Detector {
    /// Each parameter and the width of its range.
    params: Vec<(u32, f64)>,
    baseline: HashMap<u32, f64>,
    /// Moved between the first two passes without anyone touching it.
    restless: HashSet<u32>,
    pass: usize,
}

impl Detector {
    pub fn new(params: Vec<(u32, f64)>) -> Self {
        Self {
            params,
            baseline: HashMap::new(),
            restless: HashSet::new(),
            pass: 0,
        }
    }

    /// The parameters to read, in order.
    pub fn ids(&self) -> Vec<u32> {
        self.params.iter().map(|(id, _)| *id).collect()
    }

    /// Call after reading every parameter once.
    pub fn end_pass(&mut self) {
        self.pass += 1;
    }

    fn moved(&self, id: u32, value: f64) -> bool {
        let span = self
            .params
            .iter()
            .find(|(i, _)| *i == id)
            .map_or(1.0, |(_, s)| *s);
        self.baseline
            .get(&id)
            .is_some_and(|b| (value - b).abs() > span.max(1e-9) * MOVED)
    }

    /// Hands over a value just read. Returns the parameter that was touched, once it is known.
    pub fn feed(&mut self, id: u32, value: f64) -> Option<u32> {
        if !value.is_finite() {
            return None;
        }
        match self.pass {
            0 => {
                self.baseline.insert(id, value);
                None
            }
            1 => {
                if self.moved(id, value) {
                    self.restless.insert(id);
                }
                None
            }
            _ => (!self.restless.contains(&id) && self.moved(id, value)).then_some(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin's parameters, read pass after pass.
    fn read(d: &mut Detector, values: &HashMap<u32, f64>) -> Option<u32> {
        let mut found = None;
        for id in d.ids() {
            found = found.or(d.feed(id, values[&id]));
        }
        d.end_pass();
        found
    }

    fn plugin() -> (Detector, HashMap<u32, f64>) {
        let params = vec![(1, 1.0), (2, 1.0), (3, 127.0), (4, 1.0)];
        let values = HashMap::from([(1, 0.5), (2, 0.1), (3, 64.0), (4, 0.9)]);
        (Detector::new(params), values)
    }

    #[test]
    fn the_parameter_that_moves_is_the_one() {
        let (mut d, mut v) = plugin();
        assert_eq!(read(&mut d, &v), None, "remembering");
        assert_eq!(read(&mut d, &v), None, "calibrating");
        assert_eq!(read(&mut d, &v), None, "nobody has touched anything");
        v.insert(3, 90.0);
        assert_eq!(read(&mut d, &v), Some(3));
    }

    #[test]
    fn a_parameter_that_moves_on_its_own_is_ignored() {
        let (mut d, mut v) = plugin();
        read(&mut d, &v);
        // Parameter 1 is a modulated value: it moves between the first two passes...
        v.insert(1, 0.6);
        read(&mut d, &v);
        // ...and keeps moving, which must not look like a touch.
        v.insert(1, 0.7);
        assert_eq!(read(&mut d, &v), None);
        // A real touch on another one still is found.
        v.insert(2, 0.8);
        assert_eq!(read(&mut d, &v), Some(2));
    }

    #[test]
    fn tiny_wobble_is_not_a_touch_but_a_small_turn_is() {
        let (mut d, mut v) = plugin();
        read(&mut d, &v);
        read(&mut d, &v);
        // Less than a fifth of a percent of the range, in a parameter 127 wide.
        v.insert(3, 64.2);
        assert_eq!(read(&mut d, &v), None);
        v.insert(3, 65.0);
        assert_eq!(read(&mut d, &v), Some(3));
    }

    #[test]
    fn it_notices_a_touch_mid_pass_and_ignores_garbage() {
        let (mut d, v) = plugin();
        read(&mut d, &v);
        read(&mut d, &v);
        assert_eq!(d.feed(4, f64::NAN), None);
        assert_eq!(
            d.feed(4, 0.1),
            Some(4),
            "0.9 -> 0.1 is a touch, found as soon as it is read"
        );
    }
}
