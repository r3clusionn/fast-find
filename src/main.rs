use clap::Parser;
use ff::{build_pattern, parse_duration, parse_size, smart_case, Candidate, Filters, Kind, Perm};
use ignore::{WalkBuilder, WalkState};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Parser)]
#[command(version, about = "Parallel file search that respects .gitignore")]
struct Cli {
    /// Regex matched against the file name (everything matches when omitted)
    pattern: Option<String>,
    /// Directories to search (default: the current directory)
    paths: Vec<PathBuf>,
    /// Treat the pattern as a literal string
    #[arg(short = 'F', long)]
    fixed_strings: bool,
    /// Case-insensitive match (default: smart case, insensitive unless the pattern has capitals)
    #[arg(short = 'i', long, conflicts_with = "case_sensitive")]
    ignore_case: bool,
    /// Case-sensitive match
    #[arg(short = 's', long)]
    case_sensitive: bool,
    /// Match the pattern against the whole path instead of the name
    #[arg(short = 'p', long)]
    full_path: bool,
    /// Only files with this extension (repeatable, no dot)
    #[arg(short = 'e', long = "ext")]
    extensions: Vec<String>,
    /// Entry type: f (file), d (directory) or l (symlink). Repeatable
    #[arg(short = 't', long = "type", value_parser = ["f", "d", "l"])]
    kinds: Vec<String>,
    /// Size filter: +10M at least, -512k at most, 4096 exactly. Repeatable, files only
    #[arg(short = 'S', long = "size", allow_hyphen_values = true)]
    sizes: Vec<String>,
    /// Modified within this long (30m, 2h, 7d, 1w)
    #[arg(long, value_name = "AGE")]
    newer: Option<String>,
    /// Modified at least this long ago
    #[arg(long, value_name = "AGE")]
    older: Option<String>,
    /// Only read-only entries
    #[arg(long, conflicts_with = "writable")]
    readonly: bool,
    /// Only writable entries
    #[arg(long)]
    writable: bool,
    /// Include hidden files and directories
    #[arg(short = 'H', long)]
    hidden: bool,
    /// Do not respect .gitignore and .ignore files
    #[arg(short = 'I', long)]
    no_ignore: bool,
    /// Follow symbolic links
    #[arg(short = 'L', long)]
    follow: bool,
    /// Maximum directory depth
    #[arg(short = 'd', long)]
    max_depth: Option<usize>,
    /// Worker threads (default: number of CPUs)
    #[arg(short = 'j', long)]
    threads: Option<usize>,
    /// Print only the number of matches
    #[arg(short = 'c', long)]
    count: bool,
    /// Separate results with NUL instead of newline
    #[arg(short = '0', long)]
    null: bool,
    /// Sort the output by path (waits for the whole walk)
    #[arg(long)]
    sort: bool,
}

/// Collects one worker's output and writes it in large chunks, so threads rarely contend on stdout.
struct Out<'a> {
    buf: Vec<u8>,
    stop: &'a AtomicBool,
}

impl Out<'_> {
    fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        if lock.write_all(&self.buf).and_then(|_| lock.flush()).is_err() {
            // Reader went away (for example `| head`): stop the walk quietly.
            self.stop.store(true, Ordering::Relaxed);
        }
        self.buf.clear();
    }
}

impl Drop for Out<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

fn build_filters(cli: &Cli) -> Result<Filters, String> {
    let mut f = Filters::default();
    if let Some(p) = &cli.pattern {
        let insensitive = cli.ignore_case || (!cli.case_sensitive && smart_case(p));
        f.pattern = Some(build_pattern(p, cli.fixed_strings, insensitive)?);
    }
    f.full_path = cli.full_path;
    f.extensions = cli.extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
    f.kinds = cli
        .kinds
        .iter()
        .map(|k| match k.as_str() {
            "f" => Kind::File,
            "d" => Kind::Dir,
            _ => Kind::Symlink,
        })
        .collect();
    for s in &cli.sizes {
        f.sizes.push(parse_size(s)?);
    }
    f.newer = cli.newer.as_deref().map(parse_duration).transpose()?;
    f.older = cli.older.as_deref().map(parse_duration).transpose()?;
    f.perm = match (cli.readonly, cli.writable) {
        (true, _) => Some(Perm::Readonly),
        (_, true) => Some(Perm::Writable),
        _ => None,
    };
    Ok(f)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("ff: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<bool, String> {
    let filters = build_filters(&cli)?;
    let roots = if cli.paths.is_empty() { vec![PathBuf::from(".")] } else { cli.paths.clone() };
    for r in &roots {
        if !r.exists() {
            return Err(format!("{}: no such file or directory", r.display()));
        }
    }
    let mut builder = WalkBuilder::new(&roots[0]);
    for r in &roots[1..] {
        builder.add(r);
    }
    builder
        .hidden(!cli.hidden)
        .follow_links(cli.follow)
        .max_depth(cli.max_depth)
        .threads(cli.threads.unwrap_or(0));
    if cli.no_ignore {
        builder.ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
    }
    // Metadata costs a syscall per entry on most platforms, so only fetch it when a filter needs it.
    let needs_meta = !filters.sizes.is_empty()
        || filters.newer.is_some()
        || filters.older.is_some()
        || filters.perm.is_some();
    let now = SystemTime::now();
    let sep = if cli.null { 0u8 } else { b'\n' };
    let count_only = cli.count;
    let sorted: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let matched = AtomicUsize::new(0);
    let unreadable = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);

    builder.build_parallel().run(|| {
        let mut out = Out { buf: Vec::with_capacity(64 * 1024), stop: &stop };
        let (filters, sorted, matched, unreadable, stop) = (&filters, &sorted, &matched, &unreadable, &stop);
        let (sort, full_path) = (cli.sort, cli.full_path);
        Box::new(move |res| {
            if stop.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match res {
                Ok(e) => e,
                Err(_) => {
                    unreadable.fetch_add(1, Ordering::Relaxed);
                    return WalkState::Continue;
                }
            };
            if entry.depth() == 0 {
                return WalkState::Continue;
            }
            let Some(ft) = entry.file_type() else { return WalkState::Continue };
            let kind = if ft.is_dir() {
                Kind::Dir
            } else if ft.is_symlink() {
                Kind::Symlink
            } else {
                Kind::File
            };
            let (mut size, mut modified, mut readonly) = (0, None, false);
            if needs_meta {
                match entry.metadata() {
                    Ok(m) => {
                        size = m.len();
                        modified = m.modified().ok();
                        readonly = m.permissions().readonly();
                    }
                    Err(_) => {
                        unreadable.fetch_add(1, Ordering::Relaxed);
                        return WalkState::Continue;
                    }
                }
            }
            let path = entry.path().to_string_lossy();
            let name = entry.file_name().to_string_lossy();
            // Full-path patterns always see forward slashes, so one pattern works everywhere.
            let slashed;
            let match_path: &str = if cfg!(windows) && full_path {
                slashed = path.replace('\\', "/");
                &slashed
            } else {
                &path
            };
            let cand = Candidate { path: match_path, name: &name, kind, size, modified, readonly };
            if !filters.matches(&cand, now) {
                return WalkState::Continue;
            }
            matched.fetch_add(1, Ordering::Relaxed);
            if count_only {
                return WalkState::Continue;
            }
            if sort {
                sorted.lock().unwrap().push(path.into_owned());
            } else {
                out.buf.extend_from_slice(path.as_bytes());
                out.buf.push(sep);
                if out.buf.len() >= 32 * 1024 {
                    out.flush();
                }
            }
            WalkState::Continue
        })
    });

    let total = matched.load(Ordering::Relaxed);
    if count_only {
        println!("{total}");
    } else if cli.sort {
        let mut v = sorted.into_inner().unwrap();
        v.sort_unstable();
        let mut buf = Vec::with_capacity(v.iter().map(|s| s.len() + 1).sum());
        for p in &v {
            buf.extend_from_slice(p.as_bytes());
            buf.push(sep);
        }
        let _ = std::io::stdout().lock().write_all(&buf);
    }
    let skipped = unreadable.load(Ordering::Relaxed);
    if skipped > 0 {
        eprintln!("ff: {skipped} entries could not be read");
    }
    Ok(total > 0)
}
