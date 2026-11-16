//! vercmp against a table the real `vercmp` (pacman 7) printed, each pair in both orders.

use std::cmp::Ordering;

use deskshell_pacman::vercmp;

const TABLE: &str = include_str!("fixtures/vercmp.tsv");

#[test]
fn matches_the_table() {
    let mut rows = 0;
    for line in TABLE.lines() {
        let mut fields = line.split('\t');
        let (Some(a), Some(b), Some(expected)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("bad row: {line:?}");
        };
        let expected = match expected {
            "-1" => Ordering::Less,
            "0" => Ordering::Equal,
            "1" => Ordering::Greater,
            other => panic!("bad result {other:?}"),
        };
        assert_eq!(vercmp(a, b), expected, "vercmp {a:?} {b:?}");
        rows += 1;
    }
    assert!(rows > 150);
}
