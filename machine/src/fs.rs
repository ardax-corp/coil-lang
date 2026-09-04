//! Host-backed filesystem helpers (`std::fs`) returning `prelude::Result` values.

use std::fs::{self as host_fs, Metadata};
use std::io::ErrorKind;
use std::time::UNIX_EPOCH;

use common::Value;

use crate::host_enum::pack_result_or_panic;
use crate::io::{
    IoErrorTag, alloc_io_error, alloc_result_err, alloc_result_ok, as_result_int, as_result_unit,
    as_result_value, value_as_string,
};
use crate::memory::{Heap, Member, ObjArray, ObjInstance, Object};

fn io_err(e: std::io::Error) -> IoErrorTag {
    IoErrorTag::from_kind(e.kind())
}

fn as_result_bool(heap: &mut Heap, r: Result<bool, IoErrorTag>) -> Value {
    match r {
        Ok(b) => alloc_result_ok(heap, Value::from(b)),
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn alloc_path_string(heap: &mut Heap, s: &str) -> Value {
    let gc = heap.intern(s.to_string());
    Value::from(gc.as_ptr() as *mut u8 as u64)
}

fn alloc_string_array(heap: &mut Heap, names: Vec<String>) -> Value {
    let elements: Vec<Value> = names.iter().map(|s| alloc_path_string(heap, s)).collect();
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

fn set_field_int(inst: &mut ObjInstance, heap: &mut Heap, name: &str, v: i64) {
    let key = heap.intern(name.to_string());
    inst.set(key, Member::Value(Value::from(v)));
}

fn set_field_bool(inst: &mut ObjInstance, heap: &mut Heap, name: &str, v: bool) {
    let key = heap.intern(name.to_string());
    inst.set(key, Member::Value(Value::from(v)));
}

/// Build a metadata record: `size`, `is_file`, `is_dir`, `is_symlink`, `modified_unix`.
fn alloc_metadata_record(heap: &mut Heap, meta: &Metadata) -> Value {
    let modified_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
    let inst = gc.as_mut();
    set_field_int(inst, heap, "size", meta.len() as i64);
    set_field_bool(inst, heap, "is_file", meta.is_file());
    set_field_bool(inst, heap, "is_dir", meta.is_dir());
    set_field_bool(inst, heap, "is_symlink", meta.file_type().is_symlink());
    set_field_int(inst, heap, "modified_unix", modified_unix);
    Value::from(obj.addr())
}

fn metadata_bool(path: &str, pred: impl Fn(&Metadata) -> bool) -> Result<bool, IoErrorTag> {
    match host_fs::metadata(path) {
        Ok(meta) => Ok(pred(&meta)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_err(e)),
    }
}

fn with_path<F>(heap: &mut Heap, path: Value, f: F) -> Value
where
    F: FnOnce(&mut Heap, &str) -> Value,
{
    match value_as_string(heap, path) {
        Ok(p) => f(heap, &p),
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            pack_result_or_panic(heap, Err(err))
        }
    }
}

fn with_two_paths<F>(heap: &mut Heap, a: Value, b: Value, f: F) -> Value
where
    F: FnOnce(&mut Heap, &str, &str) -> Value,
{
    match (value_as_string(heap, a), value_as_string(heap, b)) {
        (Ok(p0), Ok(p1)) => f(heap, &p0, &p1),
        (Err(tag), _) | (_, Err(tag)) => {
            let err = alloc_io_error(heap, tag);
            pack_result_or_panic(heap, Err(err))
        }
    }
}

pub fn fs_exists(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = match host_fs::metadata(p) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err(e)),
        };
        as_result_bool(heap, r)
    })
}

pub fn fs_is_file(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_bool(heap, metadata_bool(p, |m| m.is_file()))
    })
}

pub fn fs_is_dir(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_bool(heap, metadata_bool(p, |m| m.is_dir()))
    })
}

pub fn fs_is_symlink(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = match host_fs::symlink_metadata(p) {
            Ok(meta) => Ok(meta.file_type().is_symlink()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err(e)),
        };
        as_result_bool(heap, r)
    })
}

pub fn fs_metadata(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = host_fs::symlink_metadata(p)
            .map(|m| alloc_metadata_record(heap, &m))
            .map_err(io_err);
        as_result_value(heap, r)
    })
}

pub fn fs_create_dir(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_unit(heap, host_fs::create_dir(p).map_err(io_err))
    })
}

pub fn fs_create_dir_all(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_unit(heap, host_fs::create_dir_all(p).map_err(io_err))
    })
}

pub fn fs_remove_file(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_unit(heap, host_fs::remove_file(p).map_err(io_err))
    })
}

pub fn fs_remove_dir(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_unit(heap, host_fs::remove_dir(p).map_err(io_err))
    })
}

pub fn fs_remove_dir_all(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        as_result_unit(heap, host_fs::remove_dir_all(p).map_err(io_err))
    })
}

pub fn fs_rename(heap: &mut Heap, from: Value, to: Value) -> Value {
    with_two_paths(heap, from, to, |heap, a, b| {
        as_result_unit(heap, host_fs::rename(a, b).map_err(io_err))
    })
}

pub fn fs_copy(heap: &mut Heap, from: Value, to: Value) -> Value {
    with_two_paths(heap, from, to, |heap, a, b| {
        let r = host_fs::copy(a, b).map(|n| n as usize).map_err(io_err);
        as_result_int(heap, r)
    })
}

pub fn fs_read_link(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = host_fs::read_link(p)
            .map(|target| {
                let text = target.to_string_lossy().into_owned();
                alloc_path_string(heap, &text)
            })
            .map_err(io_err);
        as_result_value(heap, r)
    })
}

pub fn fs_symlink(heap: &mut Heap, target: Value, link: Value) -> Value {
    with_two_paths(heap, target, link, |heap, original, link_path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            as_result_unit(heap, symlink(original, link_path).map_err(io_err))
        }
        #[cfg(windows)]
        {
            let r = host_fs::metadata(original)
                .map_err(io_err)
                .and_then(|meta| {
                    if meta.is_dir() {
                        use std::os::windows::fs::symlink_dir;
                        symlink_dir(original, link_path).map_err(io_err)
                    } else {
                        use std::os::windows::fs::symlink_file;
                        symlink_file(original, link_path).map_err(io_err)
                    }
                });
            as_result_unit(heap, r)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (heap, original, link_path);
            let err = alloc_io_error(heap, IoErrorTag::Other);
            pack_result_or_panic(heap, Err(err))
        }
    })
}

pub fn fs_list_dir(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = host_fs::read_dir(p).map_err(io_err).and_then(|rd| {
            let mut names = Vec::new();
            for entry in rd {
                let entry = entry.map_err(io_err)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                names.push(name);
            }
            names.sort();
            Ok(alloc_string_array(heap, names))
        });
        as_result_value(heap, r)
    })
}

pub fn fs_realpath(heap: &mut Heap, path: Value) -> Value {
    with_path(heap, path, |heap, p| {
        let r = host_fs::canonicalize(p)
            .map(|abs| {
                let text = abs.to_string_lossy().into_owned();
                alloc_path_string(heap, &text)
            })
            .map_err(io_err);
        as_result_value(heap, r)
    })
}

fn wrong_arity(heap: &mut Heap) -> Value {
    let err = alloc_io_error(heap, IoErrorTag::InvalidInput);
    pack_result_or_panic(heap, Err(err))
}

macro_rules! fs_host_1 {
    ($host:ident, $inner:ident) => {
        pub fn $host(heap: &mut Heap, args: &[Value]) -> Value {
            match args {
                [path] => $inner(heap, *path),
                _ => wrong_arity(heap),
            }
        }
    };
}

macro_rules! fs_host_2 {
    ($host:ident, $inner:ident) => {
        pub fn $host(heap: &mut Heap, args: &[Value]) -> Value {
            match args {
                [a, b] => $inner(heap, *a, *b),
                _ => wrong_arity(heap),
            }
        }
    };
}

fs_host_1!(host_fs_exists, fs_exists);
fs_host_1!(host_fs_is_file, fs_is_file);
fs_host_1!(host_fs_is_dir, fs_is_dir);
fs_host_1!(host_fs_is_symlink, fs_is_symlink);
fs_host_1!(host_fs_metadata, fs_metadata);
fs_host_1!(host_fs_create_dir, fs_create_dir);
fs_host_1!(host_fs_create_dir_all, fs_create_dir_all);
fs_host_1!(host_fs_remove_file, fs_remove_file);
fs_host_1!(host_fs_remove_dir, fs_remove_dir);
fs_host_1!(host_fs_remove_dir_all, fs_remove_dir_all);
fs_host_1!(host_fs_read_link, fs_read_link);
fs_host_1!(host_fs_list_dir, fs_list_dir);
fs_host_1!(host_fs_realpath, fs_realpath);
fs_host_2!(host_fs_rename, fs_rename);
fs_host_2!(host_fs_copy, fs_copy);
fs_host_2!(host_fs_symlink, fs_symlink);

/// Pipeline wiring: `(registry_name, arity, host_fn)`.
pub const FS_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    ("fs_exists", 1, host_fs_exists),
    ("fs_is_file", 1, host_fs_is_file),
    ("fs_is_dir", 1, host_fs_is_dir),
    ("fs_is_symlink", 1, host_fs_is_symlink),
    ("fs_metadata", 1, host_fs_metadata),
    ("fs_create_dir", 1, host_fs_create_dir),
    ("fs_create_dir_all", 1, host_fs_create_dir_all),
    ("fs_remove_file", 1, host_fs_remove_file),
    ("fs_remove_dir", 1, host_fs_remove_dir),
    ("fs_remove_dir_all", 1, host_fs_remove_dir_all),
    ("fs_rename", 2, host_fs_rename),
    ("fs_copy", 2, host_fs_copy),
    ("fs_read_link", 1, host_fs_read_link),
    ("fs_symlink", 2, host_fs_symlink),
    ("fs_list_dir", 1, host_fs_list_dir),
    ("fs_realpath", 1, host_fs_realpath),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::IoErrorTag;
    use crate::memory::{Heap, Member};
    use std::fs;

    fn coil_string(heap: &mut Heap, s: &str) -> Value {
        alloc_path_string(heap, s)
    }

    fn member_to_value(_heap: &Heap, m: &Member) -> Value {
        match m {
            Member::Value(v) => *v,
            Member::Object(o) => Value::from(o.addr()),
        }
    }

    fn io_tag_from_u32(tag: u32) -> Option<IoErrorTag> {
        match tag {
            0 => Some(IoErrorTag::WouldBlock),
            1 => Some(IoErrorTag::NotFound),
            2 => Some(IoErrorTag::PermissionDenied),
            3 => Some(IoErrorTag::AlreadyClosed),
            4 => Some(IoErrorTag::InvalidInput),
            5 => Some(IoErrorTag::Other),
            6 => Some(IoErrorTag::NotADirectory),
            7 => Some(IoErrorTag::AlreadyExists),
            8 => Some(IoErrorTag::TimedOut),
            9 => Some(IoErrorTag::Truncated),
            10 => Some(IoErrorTag::Certificate),
            11 => Some(IoErrorTag::Handshake),
            _ => None,
        }
    }

    fn result_ok_tag(heap: &Heap, v: Value) -> Option<u32> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) if gc.as_ref().tag == 0 => Some(0),
            _ => None,
        }
    }

    fn result_err_io_tag(heap: &Heap, v: Value) -> Option<IoErrorTag> {
        let Object::Enum(outer) = heap.find_object_by_addr(v.raw() as u64)? else {
            return None;
        };
        if outer.as_ref().tag != 1 {
            return None;
        }
        let err_val = member_to_value(heap, outer.as_ref().payload.first()?);
        let Object::Enum(inner) = heap.find_object_by_addr(err_val.raw() as u64)? else {
            return None;
        };
        io_tag_from_u32(inner.as_ref().tag)
    }

    fn unwrap_ok_bool(heap: &Heap, v: Value) -> bool {
        let Object::Enum(outer) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected Result enum");
        };
        assert_eq!(outer.as_ref().tag, 0);
        member_to_value(heap, &outer.as_ref().payload[0]).as_bool()
    }

    fn instance_field_bool(heap: &mut Heap, v: Value, field: &str) -> bool {
        let Object::Instance(inst) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected instance");
        };
        let key = heap.intern(field.to_string());
        match inst.as_ref().get(key) {
            Some(Member::Value(v)) => v.as_bool(),
            _ => panic!("missing field {field}"),
        }
    }

    fn instance_field_int(heap: &mut Heap, v: Value, field: &str) -> i64 {
        let Object::Instance(inst) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected instance");
        };
        let key = heap.intern(field.to_string());
        match inst.as_ref().get(key) {
            Some(Member::Value(v)) => v.as_int(),
            _ => panic!("missing field {field}"),
        }
    }

    fn unwrap_ok_string(heap: &Heap, v: Value) -> String {
        let Object::Enum(outer) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        let payload = member_to_value(heap, &outer.as_ref().payload[0]);
        value_as_string(heap, payload).unwrap()
    }

    fn unwrap_ok_payload(heap: &Heap, v: Value) -> Value {
        let Object::Enum(outer) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(outer.as_ref().tag, 0);
        member_to_value(heap, &outer.as_ref().payload[0])
    }

    fn array_strings(heap: &Heap, v: Value) -> Vec<String> {
        let Object::Array(arr) = heap.find_object_by_addr(v.raw() as u64).unwrap() else {
            panic!("expected array");
        };
        arr.as_ref()
            .elements
            .iter()
            .map(|e| value_as_string(heap, *e).unwrap())
            .collect()
    }

    #[test]
    fn exists_and_is_dir_round_trip() {
        let base = std::env::temp_dir().join(format!("coil_fs_unit_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mut heap = Heap::default();
        let missing = coil_string(&mut heap, base.to_str().unwrap());
        let r = fs_exists(&mut heap, missing);
        assert!(!unwrap_ok_bool(&heap, r));

        let base_s = base.to_str().unwrap().to_string();
        let base_v = coil_string(&mut heap, &base_s);
        fs_create_dir_all(&mut heap, base_v);
        let p = coil_string(&mut heap, &base_s);
        let r = fs_exists(&mut heap, p);
        assert!(unwrap_ok_bool(&heap, r));
        let p = coil_string(&mut heap, &base_s);
        let r = fs_is_dir(&mut heap, p);
        assert!(unwrap_ok_bool(&heap, r));
        let p = coil_string(&mut heap, &base_s);
        let r = fs_is_file(&mut heap, p);
        assert!(!unwrap_ok_bool(&heap, r));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn metadata_reports_file_size() {
        let path = std::env::temp_dir().join(format!("coil_fs_meta_{}", std::process::id()));
        fs::write(&path, b"abcd").unwrap();
        let mut heap = Heap::default();
        let path_s = path.to_str().unwrap().to_string();
        let path_v = coil_string(&mut heap, &path_s);
        let r = fs_metadata(&mut heap, path_v);
        assert_eq!(result_ok_tag(&heap, r), Some(0));
        let Object::Enum(outer) = heap.find_object_by_addr(r.raw() as u64).unwrap() else {
            panic!();
        };
        let rec = member_to_value(&heap, &outer.as_ref().payload[0]);
        assert_eq!(instance_field_int(&mut heap, rec, "size"), 4);
        assert!(instance_field_bool(&mut heap, rec, "is_file"));
        assert!(!instance_field_bool(&mut heap, rec, "is_dir"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn list_dir_rename_copy_remove() {
        let base = std::env::temp_dir().join(format!("coil_fs_ops_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mut heap = Heap::default();
        let base_s = base.to_str().unwrap().to_string();
        let base_v = coil_string(&mut heap, &base_s);
        fs_create_dir_all(&mut heap, base_v);
        let file = base.join("a.txt");
        fs::write(&file, b"x").unwrap();

        let base_path = coil_string(&mut heap, &base_s);
        let listed = fs_list_dir(&mut heap, base_path);
        let names = array_strings(&heap, unwrap_ok_payload(&heap, listed));
        assert!(names.contains(&"a.txt".to_string()));

        let dest = base.join("b.txt");
        let file_s = file.to_str().unwrap().to_string();
        let dest_s = dest.to_str().unwrap().to_string();
        let from_v = coil_string(&mut heap, &file_s);
        let to_v = coil_string(&mut heap, &dest_s);
        fs_rename(&mut heap, from_v, to_v);
        assert!(!file.exists());
        assert!(dest.exists());

        let copy_dest = base.join("c.txt");
        let copy_s = copy_dest.to_str().unwrap().to_string();
        let from_v = coil_string(&mut heap, &dest_s);
        let to_v = coil_string(&mut heap, &copy_s);
        let copied = fs_copy(&mut heap, from_v, to_v);
        assert_eq!(result_ok_tag(&heap, copied), Some(0));

        let copy_rm = coil_string(&mut heap, &copy_s);
        fs_remove_file(&mut heap, copy_rm);
        let dest_rm = coil_string(&mut heap, &dest_s);
        fs_remove_file(&mut heap, dest_rm);
        let base_rm = coil_string(&mut heap, &base_s);
        fs_remove_dir_all(&mut heap, base_rm);
        assert!(!base.exists());
    }

    #[test]
    fn realpath_resolves_existing_path() {
        let path = std::env::temp_dir().join(format!("coil_fs_real_{}", std::process::id()));
        fs::write(&path, b"").unwrap();
        let mut heap = Heap::default();
        let path_s = path.to_str().unwrap().to_string();
        let path_v = coil_string(&mut heap, &path_s);
        let r = fs_realpath(&mut heap, path_v);
        let abs = unwrap_ok_string(&heap, r);
        assert!(std::path::Path::new(&abs).is_absolute());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn non_string_path_yields_invalid_input() {
        let mut heap = Heap::default();
        let bad = Value::from(99_i64);
        let r = fs_exists(&mut heap, bad);
        assert_eq!(result_err_io_tag(&heap, r), Some(IoErrorTag::InvalidInput));
    }

    #[cfg(unix)]
    #[test]
    fn read_link_symlink_round_trip() {
        let base = std::env::temp_dir().join(format!("coil_fs_link_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let target = base.join("target.txt");
        fs::write(&target, b"t").unwrap();
        let link = base.join("link.txt");
        let mut heap = Heap::default();
        let target_s = target.to_str().unwrap().to_string();
        let link_s = link.to_str().unwrap().to_string();
        let target_v = coil_string(&mut heap, &target_s);
        let link_v = coil_string(&mut heap, &link_s);
        fs_symlink(&mut heap, target_v, link_v);
        let link_check = coil_string(&mut heap, &link_s);
        let r = fs_is_symlink(&mut heap, link_check);
        assert!(unwrap_ok_bool(&heap, r));
        let link_read = coil_string(&mut heap, &link_s);
        let r = fs_read_link(&mut heap, link_read);
        let text = unwrap_ok_string(&heap, r);
        assert!(text.ends_with("target.txt"));
        let _ = fs::remove_file(&link);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn fs_wiring_arities() {
        assert_eq!(FS_WIRING.len(), 16);
        let by_name: std::collections::BTreeMap<&str, usize> =
            FS_WIRING.iter().map(|&(n, a, _)| (n, a)).collect();
        assert_eq!(by_name["fs_exists"], 1);
        assert_eq!(by_name["fs_rename"], 2);
        assert_eq!(by_name["fs_copy"], 2);
        assert_eq!(by_name["fs_symlink"], 2);
    }
}
