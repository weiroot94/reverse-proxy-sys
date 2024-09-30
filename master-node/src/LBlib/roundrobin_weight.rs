use std::{collections::HashMap, hash::Hash};

use super::Weight;

#[derive(Clone, Debug)]
struct RRWeightItem<T> {
    item: T,
    weight: isize,
}

// RoundrobinWeight is a struct that contains weighted items implement LVS weighted round robin
// algorithm.
//
// http://kb.linuxvirtualitem.org/wiki/Weighted_Round-Robin_Scheduling
// http://zh.linuxvirtualitem.org/node/37
#[derive(Debug, Default)]
pub struct RoundrobinWeight<T> {
    items: Vec<RRWeightItem<T>>,
    n: isize,
    gcd: isize,
    max_w: isize,
    i: isize,
    cw: isize,
}

impl<T: Clone + PartialEq + Eq + Hash> RoundrobinWeight<T> {
    pub fn new() -> Self {
        RoundrobinWeight {
            items: Vec::new(),
            n: 0,
            gcd: 0,
            max_w: 0,
            i: 0,
            cw: 0,
        }
    }
}

impl<T: Clone + PartialEq + Eq + Hash> Weight for RoundrobinWeight<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.n == 0 {
            return None;
        }

        if self.n == 1 {
            return Some(self.items[0].item.clone());
        }

        loop {
            self.i = (self.i + 1) % self.n;
            if self.i == 0 {
                self.cw -= self.gcd;
                if self.cw <= 0 {
                    self.cw = self.max_w;
                    if self.cw == 0 {
                        return None;
                    }
                }
            }

            if self.items[self.i as usize].weight >= self.cw {
                return Some(self.items[self.i as usize].item.clone());
            }
        }
    }
    // add adds a weighted item for selection.
    fn add(&mut self, item: T, weight: isize) {
        let weight_item = RRWeightItem { item, weight };

        if weight > 0 {
            if self.gcd == 0 {
                self.gcd = weight;
                self.max_w = weight;
                self.i = -1;
                self.cw = 0;
            } else {
                self.gcd = gcd(self.gcd, weight);
                if self.max_w < weight {
                    self.max_w = weight;
                }
            }
        }

        self.items.push(weight_item);
        self.n += 1;
    }

    fn remove(&mut self, item: T) {
        // Get all items as a HashMap
        let all_items = self.all();

        // Check if the item exists in the HashMap
        if let Some(&weight) = all_items.get(&item) {
            // Find the position of the item in the original items list
            if let Some(pos) = self.items.iter().position(|x| x.item == item && x.weight == weight) {
                // Remove the item
                self.items.remove(pos);
                self.n -= 1;
               // self.sum_of_weights -= weight;
            }
        }
    }

    // all returns all items.
    fn all(&self) -> HashMap<T, isize> {
        let mut rt: HashMap<T, isize> = HashMap::new();
        for w in &self.items {
            rt.insert(w.item.clone(), w.weight);
        }
        rt
    }

    // remove_all removes all weighted items.
    fn remove_all(&mut self) {
        self.items.clear();
        self.n = 0;
        self.gcd = 0;
        self.max_w = 0;
        self.i = -1;
        self.cw = 0;
    }

    // reset resets the balancing algorithm.
    fn reset(&mut self) {
        self.i = -1;
        self.cw = 0;
    }
}

#[allow(clippy::many_single_char_names)]
fn gcd(x: isize, y: isize) -> isize {
    let mut t: isize;
    let mut a = x;
    let mut b = y;
    loop {
        t = a % b;
        if t > 0 {
            a = b;
            b = t;
        } else {
            return b;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{RoundrobinWeight, Weight};
    use std::collections::HashMap;

    #[test]
    fn test_rr_weight() {
        let mut rrw: RoundrobinWeight<&str> = RoundrobinWeight::new();
        rrw.add("server1", 5);
        rrw.add("server2", 2);
        rrw.add("server3", 3);

        let mut results: HashMap<&str, usize> = HashMap::new();

        for _ in 0..100 {
            let s = rrw.next().unwrap();
            // *results.get_mut(s).unwrap() += 1;
            *results.entry(s).or_insert(0) += 1;
        }

        assert_eq!(results["server1"], 50);
        assert_eq!(results["server2"], 20);
        assert_eq!(results["server3"], 30);
    }
}
