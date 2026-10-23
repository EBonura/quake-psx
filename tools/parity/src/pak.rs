//! Reads the original PACK archive: map entity lumps and the `progs.dat` string table.
//!
//! Only data is read. No QuakeC statements are decoded or reproduced.

use std::collections::BTreeMap;

pub type Result<T> = std::result::Result<T, String>;

/// One entity in file order; keys keep their original spelling.
#[derive(Clone, Debug, Default)]
pub struct Entity {
    /// Key/value pairs in file order (a key may repeat; lookups take the first).
    pub fields: Vec<(String, String)>,
}

impl Entity {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn class(&self) -> &str {
        self.get("classname").unwrap_or("")
    }

    /// Integer value of a key the way `atoi`/`stof` reads it: a float is truncated.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map(|v| v as i64)
    }

    pub fn spawnflags(&self) -> u32 {
        self.int("spawnflags").unwrap_or(0).max(0) as u32
    }
}

fn range(data: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    data.get(offset..offset.checked_add(length).ok_or("range overflow")?)
        .ok_or_else(|| "truncated data range".to_owned())
}

fn word(data: &[u8], offset: usize) -> Result<usize> {
    Ok(u32::from_le_bytes(range(data, offset, 4)?.try_into().unwrap()) as usize)
}

/// `(name, bytes)` for every file in a PACK archive, in directory order.
pub fn pack_files(data: &[u8]) -> Result<Vec<(String, &[u8])>> {
    if range(data, 0, 4)? != b"PACK" {
        return Err("invalid PACK signature".into());
    }
    let directory = range(data, word(data, 4)?, word(data, 8)?)?;
    let mut out = Vec::new();
    for entry in directory.chunks_exact(64) {
        let end = entry[..56].iter().position(|b| *b == 0).unwrap_or(56);
        let name = std::str::from_utf8(&entry[..end])
            .map_err(|e| e.to_string())?
            .to_owned();
        out.push((name, range(data, word(entry, 56)?, word(entry, 60)?)?));
    }
    Ok(out)
}

/// The entity lump text of a version 29 BSP.
pub fn bsp_entity_text(bsp: &[u8]) -> Result<&str> {
    if word(bsp, 0)? != 29 {
        return Err("unsupported BSP version".into());
    }
    let lump = range(bsp, word(bsp, 4)?, word(bsp, 8)?)?;
    std::str::from_utf8(lump)
        .map(|t| t.trim_end_matches('\0'))
        .map_err(|e| e.to_string())
}

/// Parse an entity lump with Quake's own token rules (quoted strings, `//` comments).
pub fn parse_entities(text: &str) -> Result<Vec<Entity>> {
    let mut rest = text;
    let mut tokens = Vec::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c <= ' ');
        if rest.is_empty() {
            break;
        }
        if rest.starts_with("//") {
            rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
            continue;
        }
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"').ok_or("unterminated string")?;
            tokens.push((true, quoted[..end].to_owned()));
            rest = &quoted[end + 1..];
        } else {
            let c = rest.chars().next().unwrap();
            if c == '{' || c == '}' {
                tokens.push((false, c.to_string()));
                rest = &rest[1..];
            } else {
                return Err(format!("unexpected entity token {c:?}"));
            }
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] != (false, "{".into()) {
            return Err("entity must start with a brace".into());
        }
        i += 1;
        let mut entity = Entity::default();
        while i < tokens.len() && tokens[i] != (false, "}".into()) {
            let key = tokens[i].1.clone();
            let value = tokens.get(i + 1).ok_or("key without value")?.1.clone();
            entity.fields.push((key, value));
            i += 2;
        }
        if i >= tokens.len() {
            return Err("missing closing brace".into());
        }
        i += 1;
        out.push(entity);
    }
    Ok(out)
}

/// Every original map: name without path or extension -> entities.
pub fn original_maps(pak: &[u8]) -> Result<BTreeMap<String, Vec<Entity>>> {
    let mut out = BTreeMap::new();
    for (name, bytes) in pack_files(pak)? {
        let Some(stem) = name
            .strip_prefix("maps/")
            .and_then(|n| n.strip_suffix(".bsp"))
        else {
            continue;
        };
        out.insert(stem.to_owned(), parse_entities(bsp_entity_text(bytes)?)?);
    }
    Ok(out)
}

/// Strings of `progs.dat` that name a sound or model file. These are the names
/// the original passes to its precache calls; which class precaches which is
/// not decoded here.
pub fn progs_resource_strings(pak: &[u8]) -> Result<Vec<String>> {
    let Some((_, progs)) = pack_files(pak)?.into_iter().find(|(n, _)| n == "progs.dat") else {
        return Err("PAK has no progs.dat".into());
    };
    if word(progs, 0)? != 6 {
        return Err("unsupported progs.dat version".into());
    }
    let strings = range(progs, word(progs, 40)?, word(progs, 44)?)?;
    let mut out: Vec<String> = strings
        .split(|b| *b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .filter(|s| {
            s.ends_with(".wav") || s.ends_with(".mdl") || s.ends_with(".spr") || s.ends_with(".bsp")
        })
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}
