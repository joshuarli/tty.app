use crate::terminal::cell::Cell;

/// Ring buffer scrollback storage.
pub struct Scrollback {
    buf: Vec<Vec<Cell>>,
    capacity: usize,
    head: usize, // next write position
    len: usize,  // current number of stored rows
    generation: u64,
}

impl Scrollback {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity.min(1024)), // lazy alloc
            capacity,
            head: 0,
            len: 0,
            generation: 0,
        }
    }

    #[allow(dead_code)]
    pub fn copy_from(&mut self, source: &Self) {
        self.buf.clone_from(&source.buf);
        self.capacity = source.capacity;
        self.head = source.head;
        self.len = source.len;
        self.generation = source.generation;
    }

    /// Copy a worker-owned scrollback snapshot while reusing rows that have
    /// not changed since the previous handoff.
    #[allow(dead_code)]
    pub fn copy_incremental_from(&mut self, source: &Self) {
        if self.capacity != source.capacity
            || source.generation < self.generation
            || source.len < self.len
            || source.buf.len() < self.buf.len()
        {
            self.copy_from(source);
            return;
        }

        let delta = source.generation - self.generation;
        if delta == 0 {
            if self.head != source.head || self.len != source.len {
                self.copy_from(source);
            }
            return;
        }

        if source.buf.len() < source.capacity {
            for index in self.buf.len()..source.buf.len() {
                self.buf.push(source.buf[index].clone());
            }
        } else if self.buf.len() < source.buf.len() || delta >= source.capacity as u64 {
            self.buf.clone_from(&source.buf);
        } else {
            let capacity = source.capacity;
            let changed = delta as usize;
            let start = (source.head + capacity - changed) % capacity;
            for offset in 0..changed {
                let index = (start + offset) % capacity;
                self.buf[index].clone_from(&source.buf[index]);
            }
        }

        self.head = source.head;
        self.len = source.len;
        self.generation = source.generation;
    }

    /// Number of rows currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clear all scrollback content.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
        self.len = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Read row N from the tail (0 = most recent evicted row).
    pub fn row(&self, n: usize) -> Option<&[Cell]> {
        if n >= self.len {
            return None;
        }
        let idx = (self.head + self.len - 1 - n) % self.len;
        Some(&self.buf[idx])
    }

    /// Push a row from a slice into the scrollback. Reuses existing allocations
    /// once the ring buffer is full (zero-alloc in steady state).
    pub fn push_slice(&mut self, cells: &[Cell]) {
        if self.capacity == 0 {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.buf.len() < self.capacity {
            self.buf.push(cells.to_vec());
            self.len = self.buf.len();
        } else {
            // Reuse existing Vec allocation
            let row = &mut self.buf[self.head];
            row.clear();
            row.extend_from_slice(cells);
            self.head = (self.head + 1) % self.capacity;
            self.len = self.capacity;
        }
    }
}
