// Doc stubs for the builtin `io::fs` module (hover docs; never compiled).

/// Copies the contents of file `from` to `to`, creating or overwriting `to`.
///
/// Copies file contents and permissions; it does not copy directories.
/// Returns `Err(IoError::NotFound)` when `from` does not exist.
/// A program that reaches `copy` compiles only with both `--allow-read`
/// and `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{copy, create_dir_all, remove_dir_all};
/// use io::file::{read_text, write_text};
/// # use tests::assert;
/// let dir = "/tmp/coil_doc_fs_copy";
/// let _ = create_dir_all(dir);
/// let _ = write_text("/tmp/coil_doc_fs_copy/a.txt", "data");
/// let _ = copy("/tmp/coil_doc_fs_copy/a.txt", "/tmp/coil_doc_fs_copy/b.txt");
/// let text = match read_text("/tmp/coil_doc_fs_copy/b.txt") {
///     Result::Ok(t) => t,
///     Result::Err(_) => "",
/// };
/// let _ = remove_dir_all(dir);
/// assert!(text == "data");
/// ```
fn copy(string from, string to) -> Result<unit, IoError> {}

/// Creates the directory `path`.
///
/// The parent must already exist (else `Err(IoError::NotFound)`), and
/// `path` must not (else `Err(IoError::AlreadyExists)`). Use
/// `create_dir_all` to create missing parents too.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::fs::{create_dir, is_dir, remove_dir};
/// # use tests::assert;
/// let dir = "/tmp/coil_doc_fs_create_dir";
/// let made = match create_dir(dir) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let again = match create_dir(dir) {
///     Result::Err(IoError::AlreadyExists) => true,
///     default => false,
/// };
/// let dir_now = match is_dir(dir) {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let _ = remove_dir(dir);
/// assert!(made && again && dir_now);
/// ```
fn create_dir(string path) -> Result<unit, IoError> {}

/// Creates the directory `path` and any missing parent directories.
///
/// Succeeds when `path` already exists as a directory.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir_all, is_dir, remove_dir_all};
/// # use tests::assert;
/// let made = match create_dir_all("/tmp/coil_doc_fs_cda/a/b/c") {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let dir_now = match is_dir("/tmp/coil_doc_fs_cda/a/b/c") {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let _ = remove_dir_all("/tmp/coil_doc_fs_cda");
/// assert!(made && dir_now);
/// ```
fn create_dir_all(string path) -> Result<unit, IoError> {}

/// Returns whether anything exists at `path`.
///
/// Follows symbolic links, so a link whose target is missing gives
/// `Ok(false)`. A missing path is `Ok(false)`, not an error; other OS
/// errors (such as `PermissionDenied`) are returned as `Err`.
/// Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{exists};
/// # use tests::assert;
/// let tmp = match exists("/tmp") {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let missing = match exists("/tmp/coil_doc_fs_no_such_file") {
///     Result::Ok(b) => b,
///     Result::Err(_) => true,
/// };
/// assert!(tmp && !missing);
/// ```
fn exists(string path) -> Result<bool, IoError> {}

/// Returns whether `path` is a directory, following symbolic links.
///
/// A missing path is `Ok(false)`. Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{is_dir};
/// # use tests::assert;
/// let tmp = match is_dir("/tmp") {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// assert!(tmp);
/// ```
fn is_dir(string path) -> Result<bool, IoError> {}

/// Returns whether `path` is a regular file, following symbolic links.
///
/// A missing path is `Ok(false)`. Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{is_file, remove_file};
/// use io::file::{write_text};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_fs_is_file.txt";
/// let _ = write_text(path, "x");
/// let file = match is_file(path) {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let dir = match is_file("/tmp") {
///     Result::Ok(b) => b,
///     Result::Err(_) => true,
/// };
/// let _ = remove_file(path);
/// assert!(file && !dir);
/// ```
fn is_file(string path) -> Result<bool, IoError> {}

/// Returns whether `path` itself is a symbolic link (the link is not followed).
///
/// A missing path is `Ok(false)`. Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir, is_symlink, remove_dir, remove_file, symlink};
/// # use tests::assert;
/// // A directory of our own: `/tmp` itself is a link on macOS.
/// let dir = "/tmp/coil_doc_fs_is_symlink_dir";
/// let link = "/tmp/coil_doc_fs_is_symlink";
/// let _ = create_dir(dir);
/// let _ = remove_file(link);
/// let _ = symlink(dir, link);
/// let yes = match is_symlink(link) {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let no = match is_symlink(dir) {
///     Result::Ok(b) => b,
///     Result::Err(_) => true,
/// };
/// let _ = remove_file(link);
/// let _ = remove_dir(dir);
/// assert!(yes && !no);
/// ```
fn is_symlink(string path) -> Result<bool, IoError> {}

/// Returns the names of the entries in directory `path`, sorted.
///
/// Only the entry names are returned (not full paths), without `.` and
/// `..`. Returns `Err(IoError::NotFound)` for a missing path and
/// `Err(IoError::NotADirectory)` when `path` is a file.
/// Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir_all, list_dir, remove_dir_all};
/// use io::file::{write_text};
/// # use tests::assert;
/// let dir = "/tmp/coil_doc_fs_list_dir";
/// let _ = create_dir_all(dir);
/// let _ = write_text("/tmp/coil_doc_fs_list_dir/b.txt", "");
/// let _ = write_text("/tmp/coil_doc_fs_list_dir/a.txt", "");
/// let names = match list_dir(dir) {
///     Result::Ok(v) => v,
///     Result::Err(_) => Vec::new(),
/// };
/// let _ = remove_dir_all(dir);
/// assert!(len(names) == 2);
/// assert!(names[0] == "a.txt" && names[1] == "b.txt");
/// ```
fn list_dir(string path) -> Result<Vec<string>, IoError> {}

/// Returns facts about `path` as a record, without following a final symbolic link.
///
/// Fields: `size` (bytes), `is_file`, `is_dir`, `is_symlink`, and
/// `modified_unix` (last modification time in whole seconds since the
/// Unix epoch, or `0` when the OS does not report it). For a symbolic
/// link the record describes the link itself, so `is_symlink` is `true`.
/// Returns `Err(IoError::NotFound)` for a missing path.
/// Needs `--allow-read`.
///
/// The record type has no name: `Metadata` in the signature above stands
/// for `{ size: int, is_file: bool, is_dir: bool, is_symlink: bool, modified_unix: int }`.
///
/// # Examples
///
/// ```
/// use io::fs::{metadata, remove_file};
/// use io::file::{write_text};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_fs_metadata.txt";
/// let _ = write_text(path, "abcd");
/// let (size, file) = match metadata(path) {
///     Result::Ok(m) => (m.size, m.is_file),
///     Result::Err(_) => (0, false),
/// };
/// let _ = remove_file(path);
/// assert!(size == 4 && file);
/// ```
fn metadata(string path) -> Result<Metadata, IoError> {}

/// Returns the target stored in the symbolic link at `path`.
///
/// The target is returned as written when the link was made (it may be
/// relative and need not exist). Returns `Err(IoError::InvalidInput)` (or
/// another error, depending on the OS) when `path` is not a link.
/// Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{read_link, remove_file, symlink};
/// # use tests::assert;
/// let link = "/tmp/coil_doc_fs_read_link";
/// let _ = remove_file(link);
/// let _ = symlink("some/target.txt", link);
/// let target = match read_link(link) {
///     Result::Ok(t) => t,
///     Result::Err(_) => "",
/// };
/// let _ = remove_file(link);
/// assert!(target == "some/target.txt");
/// ```
fn read_link(string path) -> Result<string, IoError> {}

/// Returns the canonical absolute form of `path`, with every symbolic link, `.` and `..` resolved.
///
/// The path must exist; otherwise returns `Err(IoError::NotFound)`.
/// Needs `--allow-read`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir_all, realpath, remove_dir_all};
/// # use tests::assert;
/// let _ = create_dir_all("/tmp/coil_doc_fs_realpath/sub");
/// let p = match realpath("/tmp/coil_doc_fs_realpath/sub/..") {
///     Result::Ok(p) => p,
///     Result::Err(_) => "",
/// };
/// let q = match realpath("/tmp/coil_doc_fs_realpath") {
///     Result::Ok(p) => p,
///     Result::Err(_) => "?",
/// };
/// let _ = remove_dir_all("/tmp/coil_doc_fs_realpath");
/// assert!(p == q);
/// ```
fn realpath(string path) -> Result<string, IoError> {}

/// Removes the empty directory `path`.
///
/// Fails when the directory is not empty; use `remove_dir_all` for that.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir, exists, remove_dir};
/// # use tests::assert;
/// let dir = "/tmp/coil_doc_fs_remove_dir";
/// let _ = create_dir(dir);
/// let removed = match remove_dir(dir) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let left = match exists(dir) {
///     Result::Ok(b) => b,
///     Result::Err(_) => true,
/// };
/// assert!(removed && !left);
/// ```
fn remove_dir(string path) -> Result<unit, IoError> {}

/// Removes the directory `path` and everything inside it.
///
/// Symbolic links inside are removed, not followed. Returns
/// `Err(IoError::NotFound)` when `path` does not exist.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{create_dir_all, exists, remove_dir_all};
/// use io::file::{write_text};
/// # use tests::assert;
/// let _ = create_dir_all("/tmp/coil_doc_fs_rda/x/y");
/// let _ = write_text("/tmp/coil_doc_fs_rda/x/y/f.txt", "z");
/// let removed = match remove_dir_all("/tmp/coil_doc_fs_rda") {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let left = match exists("/tmp/coil_doc_fs_rda") {
///     Result::Ok(b) => b,
///     Result::Err(_) => true,
/// };
/// assert!(removed && !left);
/// ```
fn remove_dir_all(string path) -> Result<unit, IoError> {}

/// Removes the file (or symbolic link) at `path`.
///
/// A symbolic link is removed itself, not its target. Returns
/// `Err(IoError::NotFound)` when nothing is there.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::{IoError};
/// use io::fs::{remove_file};
/// use io::file::{write_text};
/// # use tests::assert;
/// let path = "/tmp/coil_doc_fs_remove_file.txt";
/// let _ = write_text(path, "x");
/// let removed = match remove_file(path) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let again = match remove_file(path) {
///     Result::Err(IoError::NotFound) => true,
///     default => false,
/// };
/// assert!(removed && again);
/// ```
fn remove_file(string path) -> Result<unit, IoError> {}

/// Renames (moves) `from` to `to`, replacing `to` if it is a file.
///
/// Works for files and directories. Moving across file systems fails.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{exists, remove_file, rename};
/// use io::file::{write_text};
/// # use tests::assert;
/// let _ = write_text("/tmp/coil_doc_fs_rename_a.txt", "x");
/// let moved = match rename("/tmp/coil_doc_fs_rename_a.txt", "/tmp/coil_doc_fs_rename_b.txt") {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let new_there = match exists("/tmp/coil_doc_fs_rename_b.txt") {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let _ = remove_file("/tmp/coil_doc_fs_rename_b.txt");
/// assert!(moved && new_there);
/// ```
fn rename(string from, string to) -> Result<unit, IoError> {}

/// Creates a symbolic link at `link` that points to `target`.
///
/// `target` is stored as given and need not exist. Returns
/// `Err(IoError::AlreadyExists)` when `link` exists. On Windows the target
/// must exist so the right kind of link (file or directory) can be made.
/// Needs `--allow-write`.
///
/// # Examples
///
/// ```
/// use io::fs::{is_dir, remove_file, symlink};
/// # use tests::assert;
/// let link = "/tmp/coil_doc_fs_symlink";
/// let _ = remove_file(link);
/// let made = match symlink("/tmp", link) {
///     Result::Ok(_) => true,
///     Result::Err(_) => false,
/// };
/// let points_to_dir = match is_dir(link) {
///     Result::Ok(b) => b,
///     Result::Err(_) => false,
/// };
/// let _ = remove_file(link);
/// assert!(made && points_to_dir);
/// ```
fn symlink(string target, string link) -> Result<unit, IoError> {}
