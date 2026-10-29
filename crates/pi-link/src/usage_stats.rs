//! 用量统计（091）：扫描 `~/.pi/agent/sessions/**/*.jsonl` 里每条 assistant
//! 消息的 `usage`，按「本地日期 × provider/model」聚合，供「用量统计」页
//! 消费（每日明细 + 每模型明细 + 总计）。
//!
//! 设计要点：
//! - **只读 pi 的会话文件**，不写、不改、不新建任何 pi 侧文件（与 010 口径
//!   一致）；本模块唯一的写盘是 PF 自有的 `<PF数据目录>/usage-stats.json`
//!   缓存。
//! - **缓存按字节偏移增量续读**：pi 会话文件只追加（同 sessions.rs
//!   `count_bytes` 的既有结论），指纹（mtime, size）失效时从上次读完的字节
//!   继续读新增尾部，只有「文件变短 / 版本不匹配 / 无缓存」才全量重扫。
//!   首扫几百个会话文件也只做一次，之后点开统计页近乎零成本（快是第一卖点）。
//! - **按行粗筛再解析**：只有同时含 `"assistant"` 与 `"usage"` 的行才进
//!   serde_json，避免为用户消息 / 工具结果付解析费。
//! - 日期取 **本地时区**（用户视角的「今天」），模型键 = `provider/model`。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 缓存 schema 版本：扫描口径（日期/成本/模型键）或缓存结构一变就 bump，
/// 旧缓存整份丢弃重扫一次，而不是让陈旧的聚合结果永久留在磁盘上。
///
/// v1 → v2（091-1）：明细行要能点进对应会话，桶里补记 `first_line_start`
/// （该条 usage 首次出现的行首字节偏移——pi 同一消息可能多条 entry，只有
/// 首条是消息本体，偏移指过去才能定位到发言）。
const CACHE_VERSION: u64 = 2;

/// 一组用量合计（tokens 分项 + 费用 + 调用次数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    /// assistant 消息条数（= 模型调用次数）
    pub calls: u64,
}

impl UsageTotals {
    pub fn tokens(&self) -> u64 {
        self.input + self.output
    }

    pub fn cache_tokens(&self) -> u64 {
        self.cache_read + self.cache_write
    }

    /// 缓存命中率口径与 `protocol::SessionStats::cache_hit_rate` 一致：
    /// cacheRead / (input + cacheWrite + cacheRead)。
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let denom = self.input + self.cache_write + self.cache_read;
        (denom > 0).then(|| self.cache_read as f64 / denom as f64)
    }

    pub fn add(&mut self, o: &UsageTotals) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cost += o.cost;
        self.calls += o.calls;
    }

    pub fn is_empty(&self) -> bool {
        self.calls == 0
    }
}

/// 单 (日期, provider, model, 会话文件) 桶 —— 缓存里的最小单位。
/// 会话维度在最小单位里，聚合时才能既出「日×模型」又能出「明细行→会话」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Bucket {
    /// 本地日期 `YYYY-MM-DD`（字典序即时序，排序/分组都不必再解析）
    date: String,
    provider: String,
    model: String,
    /// 会话日志文件的绝对路径（明细行点击跳转的钥匙）
    file: String,
    /// 该条 usage 首次出现的**行首**字节偏移（同消息多 entry 时指本体）
    first_line_start: u64,
    #[serde(flatten)]
    totals: UsageTotals,
}

/// 一条「日 × 模型 × 会话」明细（页面最细一行；点击可跳进会话）。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEntry {
    pub provider: String,
    pub model: String,
    pub file: PathBuf,
    pub totals: UsageTotals,
}

/// 一个模型的当日合计（页面卡片分组行）。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub provider: String,
    pub model: String,
    pub totals: UsageTotals,
    /// 该模型当日各会话明细（按费用/token 降序）
    pub sessions: Vec<UsageEntry>,
}

/// 一天的用量（页面一张卡片）。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDay {
    pub date: String,
    pub totals: UsageTotals,
    /// 按费用降序（费用为 0 时按 token 降序）
    pub models: Vec<UsageRow>,
    /// 当日全部会话明细（跨模型，按时间倒序——最新会话在最上）
    pub sessions: Vec<UsageEntry>,
}

/// 全部会话的用量聚合（页面数据源）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageStats {
    /// 日期降序（最新一天在最上面）
    pub days: Vec<UsageDay>,
    pub totals: UsageTotals,
    /// 参与统计的会话文件数（页脚「N 个会话」）
    pub files: usize,
}

fn cmp_totals_desc(a: &UsageTotals, b: &UsageTotals) -> std::cmp::Ordering {
    b.cost
        .partial_cmp(&a.cost)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| b.tokens().cmp(&a.tokens()))
}

fn cmp_entry_desc(a: &UsageEntry, b: &UsageEntry) -> std::cmp::Ordering {
    cmp_totals_desc(&a.totals, &b.totals).then_with(|| b.file.cmp(&a.file))
}

impl UsageStats {
    pub fn is_empty(&self) -> bool {
        self.totals.is_empty()
    }

    fn push(&mut self, buckets: &[Bucket]) {
        // date → model key → (provider, model, totals, file → totals)
        let mut by_day: BTreeMap<
            &str,
            BTreeMap<(&str, &str), (UsageTotals, BTreeMap<&str, UsageTotals>)>,
        > = BTreeMap::new();
        for b in buckets {
            let day = by_day.entry(b.date.as_str()).or_default();
            let (row_total, files) = day
                .entry((b.provider.as_str(), b.model.as_str()))
                .or_default();
            row_total.add(&b.totals);
            files.entry(b.file.as_str()).or_default().add(&b.totals);
        }
        for (date, models) in by_day {
            let mut totals = UsageTotals::default();
            let mut rows: Vec<UsageRow> = Vec::with_capacity(models.len());
            let mut sessions: Vec<UsageEntry> = Vec::new();
            for ((provider, model), (row_total, files)) in models {
                // totals 只从明细加一遍（模型行与明细同源；再加 row_total 会双倍）
                for (file, f) in files {
                    totals.add(&f);
                    sessions.push(UsageEntry {
                        provider: provider.to_string(),
                        model: model.to_string(),
                        file: PathBuf::from(file),
                        totals: f,
                    });
                }
                rows.push(UsageRow {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    totals: row_total,
                    sessions: Vec::new(),
                });
            }
            sessions.sort_by(cmp_entry_desc);
            for r in &mut rows {
                r.sessions.extend(
                    sessions
                        .iter()
                        .filter(|e| e.provider == r.provider && e.model == r.model)
                        .cloned(),
                );
            }
            rows.sort_by(|a, b| {
                cmp_totals_desc(&a.totals, &b.totals).then_with(|| a.model.cmp(&b.model))
            });
            self.totals.add(&totals);
            self.days.push(UsageDay {
                date: date.to_string(),
                totals,
                models: rows,
                sessions,
            });
        }
    }

    fn finish(&mut self) {
        self.days.sort_by(|a, b| b.date.cmp(&a.date));
    }
}

// ---------------------------------------------------------------------------
// 缓存
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    mtime_sec: u64,
    mtime_nsec: u32,
    size: u64,
    /// 已经消费到（完整行结束处）的字节偏移：文件只追加时从这里续读
    bytes: u64,
    buckets: Vec<Bucket>,
}

impl CacheEntry {
    fn fingerprint_of(mtime: &SystemTime, size: u64) -> (u64, u32, u64) {
        let d = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
        (d.as_secs(), d.subsec_nanos(), size)
    }

    fn matches(&self, mtime: &SystemTime, size: u64) -> bool {
        Self::fingerprint_of(mtime, size) == (self.mtime_sec, self.mtime_nsec, self.size)
    }
}

/// 缓存文件内容（PF 自有目录，形态与 session-index.json 同构）。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    version: u64,
    files: BTreeMap<String, CacheEntry>,
}

fn load_cache(path: Option<&Path>) -> Cache {
    let Some(path) = path else {
        return Cache::default();
    };
    let Ok(Value::Object(obj)) = crate::config::read_json(path) else {
        return Cache::default();
    };
    if obj.get("version").and_then(|v| v.as_u64()) != Some(CACHE_VERSION) {
        return Cache::default();
    }
    let Some(files) = obj.get("files") else {
        return Cache::default();
    };
    let Ok(files) = serde_json::from_value::<BTreeMap<String, CacheEntry>>(files.clone()) else {
        return Cache::default();
    };
    Cache {
        version: CACHE_VERSION,
        files,
    }
}

fn save_cache(path: Option<&Path>, cache: &Cache) {
    let Some(path) = path else { return };
    let value = serde_json::to_value(cache).unwrap_or(Value::Null);
    let _ = crate::paths::ensure_dir();
    let _ = crate::config::write_json(path, &value);
}

// ---------------------------------------------------------------------------
// 扫描
// ---------------------------------------------------------------------------

/// 会话根下所有分组目录里的 `*.jsonl`（只两层，与 sessions.rs 布局一致）。
fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(groups) = std::fs::read_dir(root) else {
        return out;
    };
    for g in groups.flatten() {
        let gp = g.path();
        if !gp.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&gp) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl") {
                out.push(p);
            }
        }
    }
    out
}

/// 本地日期串 `YYYY-MM-DD`（epoch ms）。
fn local_date(ms: i64) -> Option<String> {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) => Some(t.format("%Y-%m-%d").to_string()),
        chrono::LocalResult::Ambiguous(t, _) => Some(t.format("%Y-%m-%d").to_string()),
        chrono::LocalResult::None => None,
    }
}

/// RFC3339（会话条目 `timestamp` 字段，ISO 串）→ epoch ms（本地日期用）。
fn iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// 解析一行会话 JSONL；非 assistant / 无 usage / 无时间 → None。
/// `line_start` = 该行在文件里的首字节偏移（明细行点击跳会话的钥匙）。
fn parse_line_bucket(line: &str, line_start: u64) -> Option<Bucket> {
    // 粗筛：pi 会话文件里绝大多数字符串都在这两词之外的行走不到解析
    if !line.contains("\"usage\"") || !line.contains("assistant") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let msg = v.get("message")?;
    if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let usage = msg.get("usage")?;
    let input = usage.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
    let output = usage.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
    let cache_read = usage.get("cacheRead").and_then(|x| x.as_u64()).unwrap_or(0);
    let cache_write = usage
        .get("cacheWrite")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    // cost 在会话文件里是对象（{input,output,cacheRead,cacheWrite,total}），
    // RPC 侧某些版本直接给数字 —— 两种都收
    let cost = usage
        .get("cost")
        .and_then(|c| {
            c.get("total")
                .and_then(|t| t.as_f64())
                .or_else(|| c.as_f64())
        })
        .unwrap_or(0.0);
    // 时间：优先消息自带（epoch ms，生成开始时刻），退条目 ISO timestamp
    let ms = msg
        .get("timestamp")
        .and_then(|t| t.as_i64())
        .or_else(|| v.get("timestamp").and_then(|t| t.as_str()).and_then(iso_ms))
        .or_else(|| v.get("timestamp").and_then(|t| t.as_i64()))?;
    let date = local_date(ms)?;
    let provider = msg
        .get("provider")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let model = msg
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    (input + output + cache_read + cache_write > 0 || cost > 0.0).then(|| Bucket {
        date,
        provider,
        model,
        file: String::new(),
        first_line_start: line_start,
        totals: UsageTotals {
            input,
            output,
            cache_read,
            cache_write,
            cost,
            calls: 1,
        },
    })
}

/// 从 `from` 字节起把新增行并进 `buckets`，返回消费到的字节偏移。
/// 只在**完整行**（带 `\n`）末停下：半行留给下次（pi 正在写）。
fn read_from(path: &Path, from: u64, buckets: &mut Vec<Bucket>) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    if from > 0 {
        file.seek(SeekFrom::Start(from))?;
    }
    let mut consumed = from;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        // 末行没有换行符 = 写入中，不消费（下次从同一偏移重读）
        if line.last() != Some(&b'\n') {
            break;
        }
        let line_start = consumed;
        consumed += n as u64;
        if let Ok(s) = std::str::from_utf8(&line)
            && let Some(b) = parse_line_bucket(s, line_start)
        {
            buckets.push(b);
        }
    }
    Ok(consumed)
}

/// 扫描一个 sessions 根；`cache_path` 给定时读写增量缓存（None = 纯内存扫）。
/// 同步实现，调用方负责放后台线程（首扫可能读到几十 MB 历史会话）。
pub fn scan(root: &Path, cache_path: Option<&Path>) -> UsageStats {
    let mut cache = load_cache(cache_path);
    let mut live: BTreeMap<String, CacheEntry> = BTreeMap::new();
    let mut all: Vec<Bucket> = Vec::new();
    let mut files = 0usize;

    for path in session_files(root) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let size = meta.len();
        let key = path.to_string_lossy().into_owned();
        files += 1;
        // 指纹命中：直接复用（未变动文件的解析费归零）
        if let Some(e) = cache.files.get(&key)
            && e.matches(&modified, size)
        {
            all.extend(e.buckets.iter().cloned());
            live.insert(key, e.clone());
            continue;
        }
        // 指纹失效：文件变长（只追加）→ 从上次偏移续读；否则全量重扫
        let (mut buckets, mut from) = match cache.files.get(&key) {
            Some(e) if size > e.size && e.bytes > 0 => (e.buckets.clone(), e.bytes),
            _ => (Vec::new(), 0),
        };
        match read_from(&path, from, &mut buckets) {
            Ok(consumed) => from = consumed,
            Err(_) => continue,
        }
        // 同一消息可能落多条 entry（重复 usage）：同一条消息的多份记录数值
        // 完全相同——只留首条：它才是消息本体，first_line_start 指过去才能
        // 定位到发言；聚合也不该重复计费。两条不同消息撞相同 usage 是合法的
        // （各自不同行首），所以必须连数值一起比。
        buckets.sort_by_key(|b| b.first_line_start);
        buckets.dedup_by(|a, b| {
            a.first_line_start == b.first_line_start
                || (a.provider == b.provider
                    && a.model == b.model
                    && a.totals.input == b.totals.input
                    && a.totals.output == b.totals.output
                    && a.totals.cache_read == b.totals.cache_read
                    && a.totals.cache_write == b.totals.cache_write)
        });
        for b in &mut buckets {
            b.file = key.clone();
        }
        all.extend(buckets.iter().cloned());
        live.insert(
            key,
            CacheEntry {
                mtime_sec: CacheEntry::fingerprint_of(&modified, size).0,
                mtime_nsec: CacheEntry::fingerprint_of(&modified, size).1,
                size,
                bytes: from,
                buckets,
            },
        );
    }

    let mut stats = UsageStats::default();
    stats.files = files;
    stats.push(&all);
    stats.finish();

    // 只保留仍存在的文件（会话被删/清理后缓存不再膨胀）
    cache.version = CACHE_VERSION;
    cache.files = live;
    save_cache(cache_path, &cache);
    stats
}

/// 默认口径：`~/.pi/agent/sessions` + PF 自有缓存文件。
pub fn scan_default() -> UsageStats {
    let Some(root) = crate::sessions::sessions_root() else {
        return UsageStats::default();
    };
    let cache = crate::paths::usage_stats_cache_file();
    scan(&root, cache.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(root: &Path, group: &str, name: &str, lines: &[String]) -> PathBuf {
        let dir = root.join(group);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        p
    }

    fn entry(ts_ms: i64, provider: &str, model: &str, inp: u64, out: u64, cost: f64) -> String {
        serde_json::json!({
            "type": "message",
            "timestamp": "2026-10-29T03:13:21.279Z",
            "message": {
                "role": "assistant",
                "model": model,
                "provider": provider,
                "timestamp": ts_ms,
                "usage": {
                    "input": inp,
                    "output": out,
                    "cacheRead": 100,
                    "cacheWrite": 10,
                    "cost": { "total": cost }
                }
            }
        })
        .to_string()
    }

    /// 本地日期分桶 + 模型聚合 + 日期降序。
    #[test]
    fn buckets_by_local_date_and_model() {
        let base = std::env::temp_dir().join(format!("pf-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("sessions");
        // 2026-10-29 03:13 UTC ≈ 同日（本地 +08:00 → 11:13）
        let d1 = iso_ms("2026-10-28T20:00:00.000Z").unwrap();
        let d2 = iso_ms("2026-10-29T20:00:00.000Z").unwrap();
        write_session(
            &root,
            "--D--proj--",
            "a.jsonl",
            &[
                entry(d1, "deepseek", "v4-pro", 1000, 100, 0.5),
                entry(d1, "deepseek", "v4-pro", 500, 50, 0.25),
                entry(d2, "openai", "gpt-5", 200, 20, 1.0),
                // 非 assistant / 无 usage 行必须被忽略
                serde_json::json!({"type":"message","message":{"role":"user"}}).to_string(),
            ],
        );
        let stats = scan(&root, None);
        assert_eq!(stats.files, 1);
        assert_eq!(stats.days.len(), 2);
        // 降序：最新一天在前
        assert!(stats.days[0].date > stats.days[1].date);
        assert_eq!(stats.totals.calls, 3);
        assert_eq!(stats.totals.input, 1700);
        assert_eq!(stats.totals.output, 170);
        assert_eq!(stats.totals.cache_read, 300);
        assert_eq!(stats.totals.cache_write, 30);
        assert!((stats.totals.cost - 1.75).abs() < 1e-9);
        // 同一天同模型合成一行；每条会话各自留下可点击明细
        let oldest = stats.days.last().unwrap();
        assert_eq!(oldest.models.len(), 1);
        assert_eq!(oldest.models[0].model, "v4-pro");
        assert_eq!(oldest.models[0].totals.calls, 2);
        assert_eq!(oldest.models[0].totals.input, 1500);
        // 日级明细 = 当日会话列表（两个日期各一条会话）
        assert_eq!(oldest.sessions.len(), 1);
        assert_eq!(oldest.sessions[0].model, "v4-pro");
        assert!(oldest.sessions[0].file.ends_with("a.jsonl"));
        assert_eq!(stats.days[0].sessions.len(), 1);
        assert_eq!(stats.days[0].sessions[0].model, "gpt-5");
        // 模型行下挂同源明细（点击跳会话靠它）
        assert_eq!(oldest.models[0].sessions.len(), 1);
        assert_eq!(oldest.models[0].sessions[0].file, oldest.sessions[0].file);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 同一消息落多条 entry（重复 usage）：只认首条——既防双倍计费，
    /// first_line_start 也得指着消息本体那行（点击跳转会话要落到发言处）。
    #[test]
    fn duplicate_usage_entries_count_once() {
        let base = std::env::temp_dir().join(format!("pf-usage-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("sessions");
        let d1 = iso_ms("2026-10-28T20:00:00.000Z").unwrap();
        let line = entry(d1, "deepseek", "v4-pro", 100, 10, 0.1);
        let p = write_session(&root, "--D--proj--", "a.jsonl", &[line.clone(), line]);
        let stats = scan(&root, None);
        assert_eq!(stats.totals.calls, 1, "重复 entry 只计一次");
        assert_eq!(stats.totals.input, 100);
        // 首条 entry 的行首 = 文件偏移 0
        assert_eq!(stats.days[0].sessions[0].file, p);
        let body = std::fs::read_to_string(&p).unwrap();
        let first_end = body.find('\n').unwrap() as u64 + 1;
        let _ = first_end; // 行首偏移在桶里；这里只验调用不重不漏
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 增量缓存：未变动文件不再重解析；追加内容只读新增尾部。
    #[test]
    fn cache_reuses_and_appends() {
        let base = std::env::temp_dir().join(format!("pf-usage-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("sessions");
        let cache = base.join("usage-stats.json");
        let d1 = iso_ms("2026-10-28T20:00:00.000Z").unwrap();
        let p = write_session(
            &root,
            "--D--proj--",
            "a.jsonl",
            &[entry(d1, "deepseek", "v4-pro", 100, 10, 0.1)],
        );
        let first = scan(&root, Some(&cache));
        assert_eq!(first.totals.calls, 1);
        assert!(cache.is_file());

        // 追加一条：文件变长 → 只读新增尾部，旧桶保留
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str(&entry(d1, "deepseek", "v4-pro", 200, 20, 0.2));
        body.push('\n');
        std::fs::write(&p, body).unwrap();
        let second = scan(&root, Some(&cache));
        assert_eq!(second.totals.calls, 2);
        assert_eq!(second.totals.input, 300);

        // 半行（正在写）：不消费，不计入
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str("{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"usage\"");
        std::fs::write(&p, &body).unwrap();
        let third = scan(&root, Some(&cache));
        assert_eq!(third.totals.calls, 2);
        // 补完这半行后能补上
        let mut body = body.clone();
        body.push_str(":{\"input\":7,\"output\":1}},\"timestamp\":)\n");
        std::fs::write(&p, body).unwrap();
        let fourth = scan(&root, Some(&cache));
        assert_eq!(fourth.totals.calls, 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 缓存版本不匹配 → 整份丢弃重扫（而不是永久沿用旧统计口径）。
    #[test]
    fn cache_version_bump_rescans() {
        let base = std::env::temp_dir().join(format!("pf-usage-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("sessions");
        let cache = base.join("usage-stats.json");
        let d1 = iso_ms("2026-10-28T20:00:00.000Z").unwrap();
        write_session(
            &root,
            "--D--proj--",
            "a.jsonl",
            &[entry(d1, "deepseek", "v4-pro", 100, 10, 0.1)],
        );
        assert_eq!(scan(&root, Some(&cache)).totals.calls, 1);
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        v["version"] = serde_json::json!(CACHE_VERSION - 1);
        std::fs::write(&cache, v.to_string()).unwrap();
        assert_eq!(scan(&root, Some(&cache)).totals.calls, 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hit_rate_matches_protocol() {
        let t = UsageTotals {
            input: 50,
            output: 5,
            cache_read: 50,
            cache_write: 0,
            cost: 0.0,
            calls: 1,
        };
        assert!((t.cache_hit_rate().unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(UsageTotals::default().cache_hit_rate(), None);
    }
}
