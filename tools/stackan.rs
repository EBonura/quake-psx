// Static worst-case stack depth over the jal call graph of a disassembled guest ELF.
//
//   llvm-objdump -d --no-show-raw-insn quake-psx.elf > quake.dis    # nightly's llvm-tools
//   rustc -O -o stackan tools/stackan.rs
//   ENTRY=_start ./stackan quake.dis 5quake3run                      # root: a substring of the mangled name
//
// Prints the deepest chain below each root (frame sizes from the `addiu $sp,
// $sp, -N` prologues, so it is exact for fixed frames) and the recursive
// functions. It cannot see through `jalr`, and it adds chains that never run
// together (the level-load path and the gameplay path both sit under `run`),
// so it is an upper bound. The dynamic watermark is the emulator's
// `--stack-profile-log`, which `cargo quake-build ship-boot` reports.
use std::collections::{HashMap, HashSet};
use std::fs;
struct F { name: String, frame: u32, calls: Vec<u32>, indirect: u32 }
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let text = fs::read_to_string(&a[1]).unwrap();
    let mut funcs: HashMap<u32, F> = HashMap::new();
    let mut cur: Option<u32> = None;
    for line in text.lines() {
        if line.ends_with(">:") && !line.starts_with(' ') {
            if let Some((addr, rest)) = line.split_once(' ') {
                if let Ok(ad) = u32::from_str_radix(addr, 16) {
                    let name = rest.trim_start_matches('<').trim_end_matches(">:").to_string();
                    funcs.insert(ad, F { name, frame: 0, calls: vec![], indirect: 0 });
                    cur = Some(ad);
                    continue;
                }
            }
        }
        let Some(c) = cur else { continue };
        let t = line.trim_start();
        let Some((addr, rest)) = t.split_once(':') else { continue };
        if u32::from_str_radix(addr, 16).is_err() { continue; }
        let ins = rest.trim();
        let f = funcs.get_mut(&c).unwrap();
        let mut it = ins.split_whitespace();
        match it.next() {
            Some("addiu") | Some("addi") => {
                let ops: String = it.collect::<Vec<_>>().join(" ");
                if ops.starts_with("$sp, $sp, -") {
                    let n = ops["$sp, $sp, -".len()..].split_whitespace().next().unwrap();
                    let v = if let Some(h) = n.strip_prefix("0x") { u32::from_str_radix(h, 16).unwrap_or(0) } else { n.parse().unwrap_or(0) };
                    f.frame = f.frame.max(v);
                }
            }
            Some("jal") | Some("j") => {
                let op = it.next().unwrap_or("");
                let is_j = ins.starts_with("j ");
                if let Some(h) = op.strip_prefix("0x") { if let Ok(tg) = u32::from_str_radix(h, 16) {
                    let tg = if tg < 0x8000_0000 { tg | 0x8000_0000 } else { tg };
                    // j inside own function is a branch, not a call
                    f.calls.push(if is_j { tg | 1 } else { tg });
                } }
            }
            Some("jalr") => f.indirect += 1,
            _ => {}
        }
    }
    // resolve function containing target; tail j within [start,end) of same fn are branches
    let mut starts: Vec<u32> = funcs.keys().cloned().collect(); starts.sort();
    let owner = |ad: u32| -> u32 { match starts.binary_search(&ad) { Ok(i) => starts[i], Err(i) => starts[i.saturating_sub(1)] } };
    let mut edges: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&ad, f) in &funcs {
        let mut v = Vec::new();
        for &c in &f.calls {
            let tail = c & 1 == 1; let tg = c & !1;
            let o = owner(tg);
            if tail && o == ad { continue; }
            if tg == o { v.push(o); }
        }
        v.sort(); v.dedup();
        edges.insert(ad, v);
    }
    if let Ok(z) = std::env::var("ZERO") { for f in funcs.values_mut() { if f.name.contains(z.as_str()) { f.frame = 0; } } }
    if let Ok(z) = std::env::var("FRAMES") { for f in funcs.values() { if z.split(',').any(|k| f.name.contains(k)) { println!("frame {:>5} {}", f.frame, f.name.chars().take(100).collect::<String>()); } } }
    let mut memo: HashMap<u32, (u32, Vec<u32>)> = HashMap::new();
    let mut cyc: HashSet<u32> = HashSet::new();
    fn dfs(n: u32, funcs: &HashMap<u32, F>, edges: &HashMap<u32, Vec<u32>>, memo: &mut HashMap<u32, (u32, Vec<u32>)>, stack: &mut Vec<u32>, cyc: &mut HashSet<u32>) -> (u32, Vec<u32>) {
        if let Some(m) = memo.get(&n) { return m.clone(); }
        if stack.contains(&n) { cyc.insert(n); return (0, vec![]); }
        stack.push(n);
        let mut best = (0u32, vec![]);
        for &e in &edges[&n] {
            let r = dfs(e, funcs, edges, memo, stack, cyc);
            if r.0 > best.0 { best = r; }
        }
        stack.pop();
        let fr = funcs[&n].frame;
        let mut path = vec![n]; path.extend(best.1);
        let res = (fr + best.0, path);
        memo.insert(n, res.clone());
        res
    }
    let mut roots: Vec<u32> = Vec::new();
    for r in &a[2..] { for (&ad, f) in &funcs { if f.name.contains(r.as_str()) { roots.push(ad); } } }
    roots.sort(); roots.dedup();
    for r in roots {
        let mut stack = vec![];
        let (d, path) = dfs(r, &funcs, &edges, &mut memo, &mut stack, &mut cyc);
        println!("{} @{:x}: {} B (frame {}) indirect-in-root {}", funcs[&r].name, r, d, funcs[&r].frame, funcs[&r].indirect);
        let mut acc = 0;
        for p in path.iter().take(14) { acc += funcs[p].frame; println!("   {:>6} {:>6} {}", funcs[p].frame, acc, funcs[p].name.chars().take(90).collect::<String>()); }
    }
    if let Ok(r) = std::env::var("ENTRY") {
        let root = funcs.iter().find(|(_, f)| f.name == r).map(|(a, _)| *a).expect("root");
        let mut entry: HashMap<u32, u32> = HashMap::new();
        entry.insert(root, 0);
        for _ in 0..300 {
            let mut changed = false;
            let snapshot: Vec<(u32, u32)> = entry.iter().map(|(a, b)| (*a, *b)).collect();
            for (u, e) in snapshot {
                for &v in &edges[&u] {
                    if cyc.contains(&u) && cyc.contains(&v) { continue; }
                    let ne = e + funcs[&u].frame;
                    if entry.get(&v).map_or(true, |old| ne > *old) { entry.insert(v, ne); changed = true; }
                }
            }
            if !changed { break; }
        }
        for c in &cyc { println!("entry {:>6} frame {:>4}  {}", entry.get(c).copied().unwrap_or(0), funcs[c].frame, funcs[c].name.chars().take(80).collect::<String>()); }
    }
    println!("recursive functions: {}", cyc.len());
    for c in &cyc { println!("  cycle member: {}", funcs[c].name); }
    let ind: Vec<_> = funcs.values().filter(|f| f.indirect > 0).map(|f| (f.name.clone(), f.indirect)).collect();
    println!("functions with jalr: {}", ind.len());
}
