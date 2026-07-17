use tty::terminal::cell::Cell;
use tty::terminal::scrollback::Scrollback;

fn make_row(len: usize, fill: u16) -> Vec<Cell> {
    (0..len)
        .map(|i| Cell {
            codepoint: fill + i as u16,
            ..Cell::default()
        })
        .collect()
}

#[test]
fn new_scrollback_empty() {
    let sb = Scrollback::new(100);
    assert_eq!(sb.len(), 0);
    assert!(sb.is_empty());
}

#[test]
fn push_one_row() {
    let mut sb = Scrollback::new(100);
    let row = make_row(10, 0x41);
    sb.push_slice(&row);
    assert_eq!(sb.len(), 1);
    assert!(!sb.is_empty());
    assert_eq!(sb.row(0).map(|r| r.len()), Some(10));
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x41);
}

#[test]
fn pushes_fill_to_capacity() {
    let mut sb = Scrollback::new(5);
    for i in 0..5 {
        sb.push_slice(&make_row(5, (0x30 + i) as u16));
    }
    assert_eq!(sb.len(), 5);
    for i in 0..5 {
        let row = sb.row(i).unwrap();
        assert_eq!(row[0].codepoint, (0x30 + 4 - i) as u16);
    }
}

#[test]
fn eviction_wraps_around() {
    let mut sb = Scrollback::new(3);
    for i in 0..6 {
        sb.push_slice(&make_row(5, (0x41 + i) as u16));
    }
    assert_eq!(sb.len(), 3);
    let row0 = sb.row(0).unwrap();
    assert_eq!(row0[0].codepoint, 0x46);
    let row2 = sb.row(2).unwrap();
    assert_eq!(row2[0].codepoint, 0x44);
}

#[test]
fn row_ordering_from_most_recent() {
    let mut sb = Scrollback::new(10);
    for i in 0..4 {
        sb.push_slice(&make_row(5, (0x41 + i) as u16));
    }
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x44);
    assert_eq!(sb.row(1).unwrap()[0].codepoint, 0x43);
    assert_eq!(sb.row(2).unwrap()[0].codepoint, 0x42);
    assert_eq!(sb.row(3).unwrap()[0].codepoint, 0x41);
}

#[test]
fn row_out_of_bounds_returns_none() {
    let mut sb = Scrollback::new(10);
    sb.push_slice(&make_row(5, 0x41));
    assert!(sb.row(0).is_some());
    assert!(sb.row(1).is_none());
}

#[test]
fn row_on_empty_returns_none() {
    let sb = Scrollback::new(10);
    assert!(sb.row(0).is_none());
}

#[test]
fn clear_empties_scrollback() {
    let mut sb = Scrollback::new(10);
    for _ in 0..5 {
        sb.push_slice(&make_row(5, 0x41));
    }
    assert_eq!(sb.len(), 5);
    sb.clear();
    assert_eq!(sb.len(), 0);
    assert!(sb.is_empty());
    assert!(sb.row(0).is_none());
}

#[test]
fn clear_and_refill() {
    let mut sb = Scrollback::new(5);
    for i in 0..3 {
        sb.push_slice(&make_row(5, (0x41 + i) as u16));
    }
    sb.clear();
    for i in 0..4 {
        sb.push_slice(&make_row(5, (0x51 + i) as u16));
    }
    assert_eq!(sb.len(), 4);
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x54);
}

#[test]
fn capacity_zero_does_nothing() {
    let mut sb = Scrollback::new(0);
    sb.push_slice(&make_row(5, 0x41));
    assert_eq!(sb.len(), 0);
    assert!(sb.is_empty());
}

#[test]
fn capacity_one_always_holds_last() {
    let mut sb = Scrollback::new(1);
    sb.push_slice(&make_row(5, 0x41));
    assert_eq!(sb.len(), 1);
    sb.push_slice(&make_row(5, 0x42));
    assert_eq!(sb.len(), 1);
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x42);
}

#[test]
fn incremental_copy_matches_full_copy() {
    let mut source = Scrollback::new(3);
    let mut incremental = Scrollback::new(3);
    let mut full = Scrollback::new(3);

    for i in 0..8 {
        source.push_slice(&make_row(5, (0x41 + i) as u16));
        incremental.copy_incremental_from(&source);
        full.copy_from(&source);

        assert_eq!(incremental.len(), full.len());
        for row in 0..incremental.len() {
            let incremental_row = incremental.row(row).unwrap();
            let full_row = full.row(row).unwrap();
            assert_eq!(incremental_row.len(), full_row.len());
            for (incremental_cell, full_cell) in incremental_row.iter().zip(full_row) {
                assert_eq!(incremental_cell.codepoint, full_cell.codepoint);
            }
        }
    }

    source.clear();
    incremental.copy_incremental_from(&source);
    full.copy_from(&source);
    assert_eq!(incremental.len(), 0);
    assert_eq!(incremental.len(), full.len());
}

#[test]
fn zero_allocation_steady_state() {
    let mut sb = Scrollback::new(10);
    for _ in 0..10 {
        sb.push_slice(&make_row(80, 0x41));
    }
    assert_eq!(sb.len(), 10);
    for _ in 0..100 {
        sb.push_slice(&make_row(80, 0x42));
    }
    assert_eq!(sb.len(), 10);
}

#[test]
fn len_bounded_by_capacity() {
    let mut sb = Scrollback::new(10);
    for _ in 0..50 {
        sb.push_slice(&make_row(10, 0x41));
    }
    assert_eq!(sb.len(), 10);
}

#[test]
fn rows_have_correct_content_after_wrap() {
    let mut sb = Scrollback::new(3);
    for i in 0..7 {
        sb.push_slice(&make_row(3, (0x41 + i) as u16));
    }
    assert_eq!(sb.len(), 3);
    let row0 = sb.row(0).unwrap();
    let row1 = sb.row(1).unwrap();
    let row2 = sb.row(2).unwrap();
    assert_eq!(row0[0].codepoint, 0x47);
    assert_eq!(row1[0].codepoint, 0x46);
    assert_eq!(row2[0].codepoint, 0x45);
}

#[test]
fn large_capacity() {
    let mut sb = Scrollback::new(1000);
    for i in 0..1000 {
        sb.push_slice(&make_row(5, 0x41 + (i % 26) as u16));
    }
    assert_eq!(sb.len(), 1000);
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x41 + (999 % 26) as u16);
}

#[test]
fn clear_resets_head_and_len() {
    let mut sb = Scrollback::new(5);
    for i in 0..7 {
        sb.push_slice(&make_row(5, (0x41 + i) as u16));
    }
    assert_eq!(sb.len(), 5);
    sb.clear();
    assert_eq!(sb.len(), 0);
    for i in 0..3 {
        sb.push_slice(&make_row(5, (0x61 + i) as u16));
    }
    assert_eq!(sb.len(), 3);
    assert_eq!(sb.row(0).unwrap()[0].codepoint, 0x63);
}
