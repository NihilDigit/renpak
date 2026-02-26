//! Integration test: verify RPA pickle parsing against real game data.

use std::path::Path;

#[test]
fn test_read_real_rpa_index() {
    let rpa_path = Path::new("/home/spencer/Games/Eternum-0.9.5-pc/game/archive_0.09.05.rpa");
    if !rpa_path.exists() {
        eprintln!("Skipping: real RPA not found");
        return;
    }

    let mut reader = renpak_core::RpaReader::open(rpa_path).expect("open RPA");
    let index = reader.read_index().expect("parse index");

    eprintln!("Entries: {}", index.len());
    assert!(index.len() > 10000, "expected >10000 entries, got {}", index.len());

    // Spot-check known files
    let ale1 = index.get("images/01/ale 1.jpg").expect("ale 1.jpg not found");
    eprintln!("ale 1.jpg: offset={}, length={}, prefix_len={}", ale1.offset, ale1.length, ale1.prefix.len());
    assert!(ale1.length > 0);
    assert!(ale1.length < 10_000_000); // should be a few hundred KB

    // Read actual file data
    let data = reader.read_file_at(ale1).expect("read ale 1.jpg");
    assert_eq!(data.len(), ale1.length as usize + ale1.prefix.len());
    // JPEG magic bytes
    assert_eq!(&data[..2], &[0xFF, 0xD8], "not a JPEG");

    // Print a few more entries
    let mut count = 0;
    for (name, entry) in index.iter() {
        if count < 5 {
            eprintln!("  {}: offset={}, len={}", name, entry.offset, entry.length);
        }
        count += 1;
    }
}
