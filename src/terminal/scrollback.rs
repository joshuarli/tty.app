use crate::terminal::cell::Cell;

/// Ring buffer scrollback storage.
pub struct Scrollback {
    buf: Vec<Vec<Cell>>,
    capacity: usize,
    head: usize, // next write position
    len: usize,  // current number of stored rows
}

impl Scrollback {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity.min(1024)), // lazy alloc
            capacity,
            head: 0,
            len: 0,
        }
    }

    /// Push a row into the scrollback. Overwrites oldest if full.
    pub fn push(&mut self, row: Vec<Cell>) {
        if self.capacity == 0 {
            return;
        }
        if self.buf.len() < self.capacity {
            self.buf.push(row);
            self.len = self.buf.len();
        } else {
            self.buf[self.head] = row;
            self.head = (self.head + 1) % self.capacity;
            self.len = self.capacity;
        }
    }

    /// Get a row by offset from the most recent (0 = most recent).
    pub fn get(&self, offset: usize) -> Option<&Vec<Cell>> {
        if offset >= self.len {
            return None;
        }
        let idx = if self.buf.len() < self.capacity {
            // Not yet wrapped
            self.len - 1 - offset
        } else {
            // Wrapped ring buffer
            (self.head + self.capacity - 1 - offset) % self.capacity
        };
        self.buf.get(idx)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
        self.len = 0;
    }
}
