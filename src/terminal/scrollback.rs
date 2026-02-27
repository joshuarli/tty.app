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

    /// Number of rows currently stored.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
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
}
