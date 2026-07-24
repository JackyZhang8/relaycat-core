use relaycat_protocol::{FilePreview, ImageVariant};
use relaycat_workspace::{FileService, ProjectRoot};
use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

struct Fixture { root: PathBuf }
impl Fixture {
    fn new() -> Self {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("relaycat-workspace-files-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..1_005 { fs::write(root.join(format!("file-{index:04}.txt")), "x").unwrap(); }
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("blob.bin"), [0, 159, 146, 150]).unwrap();
        fs::write(root.join("huge.txt"), vec![b'x'; 512 * 1024 + 1]).unwrap();
        fs::write(root.join("icon.png"), minimal_png()).unwrap();
        Self { root }
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }

#[test]
fn directory_is_paged_and_capped() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    let page = service.list("", 0, 100).unwrap();
    assert_eq!(page.entries.len(), 100);
    assert!(page.has_more);
    assert!(page.capped);
    assert_eq!(page.next_offset, Some(100));
}

#[test]
fn previews_text_binary_large_and_image_files() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    assert!(matches!(service.read("source.rs", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::Text { language, .. } if language == "rust"));
    assert!(matches!(service.read("blob.bin", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::Binary { .. }));
    assert!(matches!(service.read("huge.txt", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::TooLarge { .. }));
    assert!(matches!(service.read("icon.png", 2 * 1024 * 1024, ImageVariant::Original).unwrap(), FilePreview::Image { mime, width: 1, height: 1, .. } if mime == "image/png"));
}

fn minimal_png() -> Vec<u8> {
    vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
         0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]
}
