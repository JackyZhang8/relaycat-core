use relaycat_protocol::{DatabaseColumn, DatabaseObject, FilePreview};
use rusqlite::{Connection, OpenFlags};
use std::{fs::File, io::Read, path::Path};

pub const DATABASE_PREVIEW_SOURCE_LIMIT: u64 = 64 * 1024 * 1024;
pub const DATABASE_PREVIEW_OBJECT_LIMIT: usize = 50;
const DATABASE_PREVIEW_COLUMN_LIMIT: usize = 50;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

pub(crate) fn has_sqlite_header(path: &Path) -> bool {
    let mut header = [0_u8; SQLITE_HEADER.len()];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map(|_| &header == SQLITE_HEADER)
        .unwrap_or(false)
}

pub(crate) fn preview(path: &Path, display_path: &str, size: u64, limit: u64) -> Option<FilePreview> {
    if !has_sqlite_header(path) { return None; }
    if size > limit {
        return Some(FilePreview::TooLarge {
            path: display_path.to_string(),
            size,
            limit,
        });
    }
    inspect(path, display_path, size).ok()
}

fn inspect(path: &Path, display_path: &str, size: u64) -> rusqlite::Result<FilePreview> {
    let uri = immutable_uri(path);
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;

    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name
         FROM sqlite_schema
         WHERE type IN ('table', 'view', 'index') AND name NOT LIKE 'sqlite_%'
         ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 1 ELSE 2 END, name
         LIMIT ?1",
    )?;
    let rows = statement.query_map([(DATABASE_PREVIEW_OBJECT_LIMIT + 1) as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut raw_objects = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = raw_objects.len() > DATABASE_PREVIEW_OBJECT_LIMIT;
    raw_objects.truncate(DATABASE_PREVIEW_OBJECT_LIMIT);
    drop(statement);

    let mut objects = Vec::with_capacity(raw_objects.len());
    for (kind, name, owning_table) in raw_objects {
        let columns = if kind == "table" || kind == "view" {
            columns(&connection, &name).unwrap_or_default()
        } else {
            Vec::new()
        };
        objects.push(DatabaseObject {
            table_name: (kind == "index").then_some(owning_table),
            name,
            kind,
            columns,
        });
    }

    Ok(FilePreview::Database {
        path: display_path.to_string(),
        format: "sqlite".to_string(),
        size,
        objects,
        has_more,
    })
}

fn immutable_uri(path: &Path) -> String {
    #[cfg(windows)]
    let display = path.to_string_lossy().replace('\\', "/");
    #[cfg(not(windows))]
    let display = path.to_string_lossy();
    let mut encoded = String::with_capacity(display.len() + 32);
    for byte in display.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file:{encoded}?mode=ro&immutable=1")
}

fn columns(connection: &Connection, object_name: &str) -> rusqlite::Result<Vec<DatabaseColumn>> {
    let mut statement = connection.prepare(
        "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid",
    )?;
    statement
        .query_map([object_name], |row| {
            let not_null = row.get::<_, i64>(2)? != 0;
            let primary_key = row.get::<_, i64>(3)? != 0;
            Ok(DatabaseColumn {
                name: row.get(0)?,
                declared_type: row.get::<_, String>(1)?,
                nullable: !not_null && !primary_key,
                primary_key,
            })
        })?
        .take(DATABASE_PREVIEW_COLUMN_LIMIT)
        .collect()
}
