use relaycat_protocol::{FilePreview, ImageVariant};
use relaycat_workspace::{
    ARCHIVE_PREVIEW_SOURCE_LIMIT, FileService, ProjectRoot, IMAGE_PREVIEW_LIMIT,
    TEXT_PREVIEW_LIMIT,
};
use std::{
    fs,
    fs::File,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture { root: PathBuf }
impl Fixture {
    fn new() -> Self {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("relaycat-workspace-files-{suffix}-{fixture_id}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..1_005 { fs::write(root.join(format!("file-{index:04}.txt")), "x").unwrap(); }
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("blob.bin"), [0, 159, 146, 150]).unwrap();
        fs::write(root.join("huge.txt"), vec![b'x'; 512 * 1024 + 1]).unwrap();
        fs::write(root.join("huge.png"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        fs::write(root.join("icon.png"), minimal_png()).unwrap();
        write_wide_png(&root.join("wide.png"));
        write_zip(&root.join("bundle.zip"), 51);
        write_unsafe_zip(&root.join("unsafe.zip"));
        write_tar(&root.join("bundle.tar"));
        write_tgz(&root.join("bundle.tar.gz"));
        fs::copy(root.join("bundle.tar.gz"), root.join("bundle.tgz")).unwrap();
        write_gz(&root.join("notes.txt.gz"));
        write_7z(&root, &root.join("bundle.7z"));
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
    assert!(page.entries[0].modified_unix_seconds.is_some());
}

#[test]
fn previews_text_binary_large_and_image_files() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    assert!(matches!(service.read("source.rs", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::Text { language, .. } if language == "rust"));
    assert!(matches!(service.read("blob.bin", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::Binary { .. }));
    assert!(matches!(service.read("huge.txt", 512 * 1024, ImageVariant::Thumbnail).unwrap(), FilePreview::TooLarge { .. }));
    assert!(matches!(service.read("huge.png", 2 * 1024 * 1024, ImageVariant::Original).unwrap(), FilePreview::TooLarge { limit, .. } if limit == 1024 * 1024));
    assert!(matches!(service.read("icon.png", 2 * 1024 * 1024, ImageVariant::Original).unwrap(), FilePreview::Image { mime, width: 1, height: 1, .. } if mime == "image/png"));
}

#[test]
fn preview_limits_keep_text_at_512_kib_and_images_at_1_mib() {
    assert_eq!(TEXT_PREVIEW_LIMIT, 512 * 1024);
    assert_eq!(IMAGE_PREVIEW_LIMIT, 1024 * 1024);
}

#[test]
fn previews_supported_archives_without_extracting_entries() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());

    for (path, expected_format) in [
        ("bundle.zip", "zip"),
        ("bundle.7z", "7z"),
        ("bundle.tar", "tar"),
        ("bundle.tar.gz", "tar.gz"),
        ("bundle.tgz", "tgz"),
    ] {
        let preview = service.read(path, 1, ImageVariant::Thumbnail).unwrap();
        assert!(matches!(preview, FilePreview::Archive { format, ref entries, .. }
            if format == expected_format && !entries.is_empty()), "{path}");
    }

    let gzip = service.read("notes.txt.gz", 1, ImageVariant::Thumbnail).unwrap();
    assert!(matches!(gzip, FilePreview::Archive { format, ref entries, has_more: false, .. }
        if format == "gz" && entries.len() == 1 && entries[0].path == "notes.txt" && entries[0].size == 5));
    assert!(!fixture.root.join("file-000.txt").exists());
}

#[test]
fn archive_preview_returns_only_first_fifty_entries() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    let preview = service.read("bundle.zip", 1, ImageVariant::Thumbnail).unwrap();
    assert!(matches!(preview, FilePreview::Archive { ref entries, has_more: true, .. }
        if entries.len() == 50 && entries[0].path == "file-000.txt" && entries[49].path == "file-049.txt"));
}

#[test]
fn archive_preview_skips_unsafe_entry_paths() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    let preview = service.read("unsafe.zip", 1, ImageVariant::Thumbnail).unwrap();
    assert!(matches!(preview, FilePreview::Archive { ref entries, has_more: false, .. }
        if entries.len() == 1 && entries[0].path == "safe/file.txt"));
}

#[test]
fn oversized_archives_are_rejected_before_parsing_except_for_gzip_metadata() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());

    for name in ["large.zip", "large.7z", "large.tar", "large.tar.gz", "large.tgz"] {
        let path = fixture.root.join(name);
        File::create(&path).unwrap().set_len(ARCHIVE_PREVIEW_SOURCE_LIMIT + 1).unwrap();
        let preview = service.read(name, 1, ImageVariant::Thumbnail).unwrap();
        assert!(matches!(preview, FilePreview::TooLarge { size, limit, .. }
            if size == ARCHIVE_PREVIEW_SOURCE_LIMIT + 1 && limit == ARCHIVE_PREVIEW_SOURCE_LIMIT), "{name}");
    }

    let gzip_path = fixture.root.join("large.gz");
    write_gz(&gzip_path);
    fs::OpenOptions::new().write(true).open(&gzip_path).unwrap()
        .set_len(ARCHIVE_PREVIEW_SOURCE_LIMIT + 1).unwrap();
    let preview = service.read("large.gz", 1, ImageVariant::Thumbnail).unwrap();
    assert!(matches!(preview, FilePreview::Archive { format, .. } if format == "gz"));
}

#[test]
fn thumbnail_variant_resizes_to_512_pixels_while_original_is_preserved() {
    let fixture = Fixture::new();
    let service = FileService::new(ProjectRoot::open(&fixture.root).unwrap());
    let source = fs::read(fixture.root.join("wide.png")).unwrap();

    let thumbnail = service.read("wide.png", 1024 * 1024, ImageVariant::Thumbnail).unwrap();
    assert!(matches!(thumbnail, FilePreview::Image { width: 512, height: 128, ref bytes, .. } if bytes != &source));

    let original = service.read("wide.png", 1024 * 1024, ImageVariant::Original).unwrap();
    assert!(matches!(original, FilePreview::Image { width: 1024, height: 256, ref bytes, .. } if bytes == &source));
}

fn minimal_png() -> Vec<u8> {
    vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
         0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]
}

fn write_zip(path: &std::path::Path, count: usize) {
    let file = File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    for index in 0..count {
        writer.start_file(format!("file-{index:03}.txt"), options).unwrap();
        writer.write_all(b"x").unwrap();
    }
    writer.finish().unwrap();
}

fn write_unsafe_zip(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    for name in ["safe/file.txt", "../escape.txt", "/absolute.txt", "C:\\absolute.txt"] {
        writer.start_file(name, options).unwrap();
        writer.write_all(b"x").unwrap();
    }
    writer.finish().unwrap();
}

fn write_tar(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut builder = tar::Builder::new(file);
    append_tar_entry(&mut builder, "folder/app.js", b"console.log(1);");
    builder.finish().unwrap();
}

fn write_tgz(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_tar_entry(&mut builder, "folder/app.js", b"console.log(1);");
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn append_tar_entry<W: Write>(builder: &mut tar::Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(1_700_000_000);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

fn write_gz(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut encoder = flate2::GzBuilder::new()
        .filename("notes.txt")
        .mtime(1_700_000_000)
        .write(file, flate2::Compression::default());
    encoder.write_all(b"hello").unwrap();
    encoder.finish().unwrap();
}

fn write_7z(root: &std::path::Path, path: &std::path::Path) {
    let source = root.join("seven.txt");
    fs::write(&source, b"seven").unwrap();
    let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
    writer.push_source_path_non_solid(&source, |_| true).unwrap();
    writer.finish().unwrap();
    fs::remove_file(source).unwrap();
}

fn write_wide_png(path: &std::path::Path) {
    let image = image::RgbImage::from_pixel(1024, 256, image::Rgb([32, 96, 192]));
    image.save(path).unwrap();
}
