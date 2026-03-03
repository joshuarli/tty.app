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

    /// Clear all scrollback content.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
        self.len = 0;
    }

    /// Push a row from a slice into the scrollback. Reuses existing allocations
    /// once the ring buffer is full (zero-alloc in steady state).
    pub fn push_slice(&mut self, cells: &[Cell]) {
        if self.capacity == 0 {
            return;
        }
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
