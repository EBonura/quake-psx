//! Execute the actual inline MIPS liquid warp against an independent scalar
//! oracle.
//!
//! This needs GNU MIPS binutils and a headless PSoXide frontend. It uses
//! synthetic inputs only, creates no retail assets, and never builds
//! diagnostic code into Quake.
//!
//! Example: quake-liquid-mips --frontend /path/to/frontend --out /tmp/liquid-check
//!
//! Set `MIPS_AS` and `MIPS_OBJCOPY` (or pass --assembler / --objcopy) for a
//! different GNU tool prefix.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, String>;

const TILE: usize = 64;
const TEXELS: usize = TILE * TILE;
const CASES: usize = 3;

/// A MIPS I-type instruction word. `value` is masked to 16 bits, so negative
/// branch offsets and immediates encode as in an assembler.
fn immediate(op: u32, rs: u32, rt: u32, value: i32) -> u32 {
    op << 26 | rs << 21 | rt << 16 | (value as u32 & 0xFFFF)
}

/// An EXE that copies the phase window to scratchpad, calls the real routine,
/// then writes a completion marker. Independent instructions satisfy each stub
/// load delay.
fn harness(routine: &[u8], source: &[u8], offsets: &[u8]) -> Vec<u8> {
    let ins: [u32; 24] = [
        immediate(15, 0, 4, 0x8010),
        immediate(15, 0, 5, 0x8010),
        immediate(13, 5, 5, 0x2000),
        immediate(15, 0, 8, 0x8010),
        immediate(13, 8, 8, 0x1000),
        immediate(15, 0, 9, 0x1F80),
        immediate(9, 0, 10, 64),
        immediate(36, 8, 11, 0),
        immediate(9, 8, 8, 1),
        immediate(40, 9, 11, 0),
        immediate(9, 9, 9, 1),
        immediate(9, 10, 10, -1),
        immediate(5, 10, 0, -6),
        0,
        immediate(9, 0, 12, 0x55),
        immediate(9, 0, 14, 0xAA),
        immediate(15, 0, 6, 0x1F80),
        3 << 26 | ((0x8002_0000u32 >> 2) & 0x03FF_FFFF),
        0,
        immediate(15, 0, 8, 0x8010),
        immediate(9, 0, 9, 0x1234),
        immediate(43, 8, 9, 0x3000),
        immediate(4, 0, 0, -1),
        0,
    ];
    let mut payload = vec![0u8; 0xF4000];
    for (index, word) in ins.iter().enumerate() {
        payload[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    payload[0x10000..0x10000 + routine.len()].copy_from_slice(routine);
    payload[0xF0000..0xF0000 + source.len()].copy_from_slice(source);
    payload[0xF1000..0xF1000 + offsets.len()].copy_from_slice(offsets);
    let mut header = vec![0u8; 2048];
    header[..8].copy_from_slice(b"PS-X EXE");
    header[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    header[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    header[0x1C..0x20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    header[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
    header.extend(payload);
    header
}

/// The quoted assembly lines of the routine's `core::arch::asm!` block.
fn asm_lines(liquid_rs: &str) -> Result<Vec<String>> {
    let after_fn = liquid_rs
        .split_once("unsafe fn warp_tile_64_mips")
        .ok_or("liquid.rs has no warp_tile_64_mips")?
        .1;
    let after_asm = after_fn
        .split_once("core::arch::asm!(")
        .ok_or("warp_tile_64_mips has no asm! block")?
        .1;
    let block = after_asm
        .split_once("in(\"$4\")")
        .ok_or("asm! block has no operand list")?
        .0;
    let line = Regex::new(r#"(?m)^[ \t\r\f\v]*"([^"\n]*)","#).map_err(|e| e.to_string())?;
    let lines: Vec<String> = line
        .captures_iter(block)
        .map(|c| c[1].to_string())
        .collect();
    if lines.is_empty() || !lines.iter().any(|l| l == ".set noreorder") {
        return Err("no assembly lines found, or no .set noreorder".into());
    }
    Ok(lines)
}

fn assembly(lines: &[String]) -> String {
    format!(
        ".text\n.globl liquid\nliquid:\n{}\n.set noreorder\njr $31\nnop\n",
        lines.join("\n")
    )
}

/// The three synthetic tiles: constant, then two patterned ones.
fn source_tile(case: usize) -> Vec<u8> {
    if case == 0 {
        return vec![37; TEXELS];
    }
    (0..TILE)
        .flat_map(|y| (0..TILE).map(move |x| (x, y)))
        .map(|(x, y)| (((x * 3 + y * 11) ^ (x >> 2) ^ (case * 19)) & 255) as u8)
        .collect()
}

fn offsets_for(case: usize) -> Vec<u8> {
    (0..TILE).map(|x| ((x * 7 + case * 3) & 15) as u8).collect()
}

/// The scalar reference the routine must match texel for texel.
fn expected_tile(source: &[u8], offsets: &[u8]) -> Vec<u8> {
    (0..TILE)
        .flat_map(|y| (0..TILE).map(move |x| (x, y)))
        .map(|(x, y)| {
            source[((y + usize::from(offsets[x])) & 63) * 64 + ((x + usize::from(offsets[y])) & 63)]
        })
        .collect()
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn run_tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed: {status}"))
    }
}

struct Options {
    frontend: PathBuf,
    out: PathBuf,
    source: PathBuf,
    assembler: String,
    objcopy: String,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let default_source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/quake-core/src/liquid.rs");
    let mut options = Options {
        frontend: PathBuf::new(),
        out: PathBuf::new(),
        source: default_source,
        assembler: std::env::var("MIPS_AS").unwrap_or_else(|_| "mipsel-none-elf-as".into()),
        objcopy: std::env::var("MIPS_OBJCOPY").unwrap_or_else(|_| "mipsel-none-elf-objcopy".into()),
    };
    let (mut frontend, mut out) = (false, false);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or(format!("{flag} needs a value"))?;
        i += 1;
        match flag {
            "--frontend" => {
                options.frontend = value.into();
                frontend = true;
            }
            "--out" => {
                options.out = value.into();
                out = true;
            }
            "--source" => options.source = value.into(),
            "--assembler" => options.assembler = value,
            "--objcopy" => options.objcopy = value,
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    if !frontend || !out {
        return Err("--frontend and --out are required".into());
    }
    Ok(options)
}

fn run(options: &Options) -> Result<()> {
    fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
    let source_text = fs::read_to_string(&options.source)
        .map_err(|e| format!("{}: {e}", options.source.display()))?;
    let lines = asm_lines(&source_text)?;
    let assembly_path = options.out.join("liquid.S");
    let object = options.out.join("liquid.o");
    let raw = options.out.join("liquid.bin");
    fs::write(&assembly_path, assembly(&lines)).map_err(|e| e.to_string())?;
    run_tool(
        &options.assembler,
        &[
            "-EL".as_ref(),
            "-mips1".as_ref(),
            "-o".as_ref(),
            object.as_os_str(),
            assembly_path.as_os_str(),
        ],
    )?;
    run_tool(
        &options.objcopy,
        &[
            "-O".as_ref(),
            "binary".as_ref(),
            "-j".as_ref(),
            ".text".as_ref(),
            object.as_os_str(),
            raw.as_os_str(),
        ],
    )?;
    let routine = fs::read(&raw).map_err(|e| e.to_string())?;

    let mut results: Vec<Value> = Vec::new();
    for case in 0..CASES {
        let source = source_tile(case);
        let offsets = offsets_for(case);
        let expected = expected_tile(&source, &offsets);
        let exe = options.out.join(format!("case-{case}.exe"));
        let ram_path = options.out.join(format!("case-{case}-ram.bin"));
        fs::write(&exe, harness(&routine, &source, &offsets)).map_err(|e| e.to_string())?;
        let command: Vec<String> = [
            options.frontend.display().to_string(),
            "launch".into(),
            "--path".into(),
            exe.display().to_string(),
            "--steps".into(),
            "250000".into(),
            "--dump-ram".into(),
            ram_path.display().to_string(),
        ]
        .to_vec();
        let log = fs::File::create(options.out.join(format!("case-{case}.log")))
            .map_err(|e| e.to_string())?;
        let status = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(log))
            .status()
            .map_err(|e| format!("cannot run {}: {e}", command[0]))?;
        if !status.success() {
            return Err(format!("frontend failed for case {case}: {status}"));
        }
        let ram = fs::read(&ram_path).map_err(|e| format!("{}: {e}", ram_path.display()))?;
        if ram.len() < 0x103004 {
            return Err("RAM dump is too short".into());
        }
        let marker = u32::from_le_bytes(ram[0x103000..0x103004].try_into().expect("four bytes"));
        if marker != 0x1234 {
            return Err("MIPS routine did not return".into());
        }
        let actual = &ram[0x102000..0x103000];
        let mismatch = actual.iter().zip(&expected).filter(|(a, b)| a != b).count();
        fs::write(options.out.join(format!("case-{case}-actual.bin")), actual)
            .map_err(|e| e.to_string())?;
        fs::write(
            options.out.join(format!("case-{case}-expected.bin")),
            &expected,
        )
        .map_err(|e| e.to_string())?;
        results.push(json!({
            "case": case,
            "mismatching_texels": mismatch,
            "texels": TEXELS,
            "command": command,
        }));
    }
    let frontend_bytes = fs::read(&options.frontend).map_err(|e| e.to_string())?;
    let result = json!({
        "source": options.source.display().to_string(),
        "source_sha256": sha256(source_text.as_bytes()),
        "routine_sha256": sha256(&routine),
        "frontend": fs::canonicalize(&options.frontend).unwrap_or_else(|_| options.frontend.clone()).display().to_string(),
        "frontend_sha256": sha256(&frontend_bytes),
        "cases": results,
    });
    let text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    fs::write(options.out.join("result.json"), text + "\n").map_err(|e| e.to_string())?;
    if result["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .any(|c| c["mismatching_texels"] != 0)
    {
        return Err(format!("texel mismatch: {result}"));
    }
    println!("MIPS liquid warp: all 12,288 texels match the scalar reference");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = parse_args(&args).and_then(|options| run(&options));
    if let Err(message) = result {
        eprintln!("quake-liquid-mips: {message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_encode_like_an_assembler() {
        // lui $4, 0x8010 ; addiu $10, $0, 64 ; bne $10, $0, -6
        assert_eq!(immediate(15, 0, 4, 0x8010), 0x3C04_8010);
        assert_eq!(immediate(9, 0, 10, 64), 0x240A_0040);
        assert_eq!(immediate(5, 10, 0, -6), 0x1540_FFFA);
        // jal 0x80020000
        assert_eq!(3 << 26 | ((0x8002_0000u32 >> 2) & 0x03FF_FFFF), 0x0C00_8000);
    }

    #[test]
    fn harness_lays_out_header_code_routine_and_inputs() {
        let routine = [1u8, 2, 3, 4];
        let source = vec![7u8; 4096];
        let offsets: Vec<u8> = (0..64).collect();
        let exe = harness(&routine, &source, &offsets);
        assert_eq!(exe.len(), 2048 + 0xF4000);
        assert_eq!(&exe[..8], b"PS-X EXE");
        assert_eq!(
            u32::from_le_bytes(exe[0x10..0x14].try_into().unwrap()),
            0x8001_0000
        );
        assert_eq!(
            u32::from_le_bytes(exe[0x1C..0x20].try_into().unwrap()),
            0xF4000
        );
        assert_eq!(
            u32::from_le_bytes(exe[0x30..0x34].try_into().unwrap()),
            0x801F_FF00
        );
        let payload = &exe[2048..];
        assert_eq!(&payload[0x10000..0x10004], &routine);
        assert_eq!(payload[0xF0000], 7);
        assert_eq!(payload[0xF1000 + 63], 63);
        assert_eq!(
            u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            0x3C04_8010
        );
    }

    #[test]
    fn asm_block_is_extracted_between_the_function_and_its_operands() {
        let source = "fn other() { \"ignored\", }\nunsafe fn warp_tile_64_mips(a: u8) {\n core::arch::asm!(\n  \".set noreorder\",\n  \"lbu $8, 0($4)\",\n  in(\"$4\") a,\n );\n}\n";
        let lines = asm_lines(source).unwrap();
        assert_eq!(lines, [".set noreorder", "lbu $8, 0($4)"]);
        assert!(assembly(&lines).starts_with(".text\n.globl liquid\nliquid:\n.set noreorder\n"));
        assert!(assembly(&lines).ends_with("jr $31\nnop\n"));
        // An asm block without .set noreorder, or no block at all, is refused.
        assert!(asm_lines(
            "unsafe fn warp_tile_64_mips() { core::arch::asm!(\"nop\", in(\"$4\") 0) }"
        )
        .is_err());
        assert!(asm_lines("fn nothing() {}").is_err());
    }

    #[test]
    fn oracle_matches_the_dense_resample() {
        let constant = source_tile(0);
        assert!(expected_tile(&constant, &offsets_for(0))
            .iter()
            .all(|&t| t == 37));
        let source = source_tile(1);
        let offsets = offsets_for(1);
        let expected = expected_tile(&source, &offsets);
        // Texel (x=5, y=9): row 9 + offsets[5], column 5 + offsets[9].
        let row = (9 + usize::from(offsets[5])) & 63;
        let column = (5 + usize::from(offsets[9])) & 63;
        assert_eq!(expected[9 * 64 + 5], source[row * 64 + column]);
        assert_eq!(source.len(), 4096);
        assert_eq!(source[0], 0 ^ 0 ^ 19);
    }
}
