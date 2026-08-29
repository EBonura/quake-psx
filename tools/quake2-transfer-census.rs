//! Corpus census for transferring Quake II PSX's resident brush renderer.
//!
//! The retail renderer's hot unit is a brush-sized range of at most 32 source
//! quads with invariant packet fields already installed.  Quake-PSX instead
//! walks an exact PVS face list in ascending source order.  This tool answers
//! the prerequisite question before a new map format is designed: how large
//! can exact-visibility, shared-edge brush ranges be without admitting hidden
//! faces, and how much source order would they disturb?

use quake_cook::{
    encode_render_quad_payload, encode_render_sections, Bsp, BspLump, PakArchive,
    RenderQuadCellInput, RenderQuadPayloadInput, RenderSectionInput,
};
use quake_formats::resident::ResidentMap;
use quake_formats::{
    LumpKind, Plane, PsbIndex, RenderQuad, RenderQuadCommand, RenderQuadFace, RenderQuadObject,
    RenderQuadRun, SliceReader, FACE_BACKSIDE, FACE_BAKED_LIGHT, FACE_BAKED_UV,
    RENDER_QUAD_COMMAND_BYTES, RENDER_QUAD_FACE_BACKSIDE, RENDER_QUAD_FACE_BYTES,
    RENDER_QUAD_HEADER_BYTES, RENDER_QUAD_OBJECT_BYTES, RENDER_QUAD_OBJECT_MAX_FACES,
    RENDER_QUAD_OBJECT_MAX_POSITIONS, RENDER_QUAD_OBJECT_MAX_QUADS, RENDER_QUAD_PACKET_BYTES,
    RENDER_QUAD_POSITION_BYTES, RENDER_QUAD_PROJECTED_POSITION_BYTES, RENDER_QUAD_RECORD_BYTES,
    RENDER_QUAD_REFERENCE_BYTES, RENDER_QUAD_RUN_BYTES, RENDER_QUAD_RUN_PATCH_CLUT,
    RENDER_SECTION_NONE, RESIDENT_MAP_ARENA_BYTES, TEXTURE_INVISIBLE, TEXTURE_LAYERED_SKY,
    TEXTURE_LIQUID, TEXTURE_NULL, TEXTURE_SKY,
};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];
const RETAIL_FACE_LIMIT: usize = 32;
const RETAIL_VERTEX_LIMIT: usize = 255;
const LEAF_BOUNDS_GRID: i16 = 32;
const LEAF_BOUNDS_GRID_SHIFT: u32 = LEAF_BOUNDS_GRID.trailing_zeros();
const GPU_ARENA_BYTES: usize = 128 * 1024;
const GPU_ARENA_SAFETY_BYTES: usize = 8 * 1024;
const RENDER_SECTION_CPU_ACTIVE_TARGET_BYTES: usize = 96 * 1024;
const RENDER_SECTION_GPU_TARGET_BYTES: usize = GPU_ARENA_BYTES - GPU_ARENA_SAFETY_BYTES;
const HOT_PREFIX_KIB: [usize; 5] = [0, 16, 32, 48, 64];

const fn encode_leaf_bound_min(value: i16) -> i8 {
    let units = (value as i32) >> LEAF_BOUNDS_GRID_SHIFT;
    if units <= i8::MIN as i32 {
        i8::MIN
    } else {
        units as i8
    }
}

const fn encode_leaf_bound_max(value: i16) -> i8 {
    let units = ((value as i32) + (LEAF_BOUNDS_GRID as i32 - 1)) >> LEAF_BOUNDS_GRID_SHIFT;
    if units >= i8::MAX as i32 {
        i8::MAX
    } else {
        units as i8
    }
}

const fn decode_leaf_bound_min(code: i8) -> i16 {
    if code == i8::MIN {
        i16::MIN
    } else {
        (code as i16) << LEAF_BOUNDS_GRID_SHIFT
    }
}

const fn decode_leaf_bound_max(code: i8) -> i16 {
    if code == i8::MAX {
        i16::MAX
    } else {
        (code as i16) << LEAF_BOUNDS_GRID_SHIFT
    }
}

#[derive(Clone, Debug)]
struct SurfaceCorner {
    position: u16,
    uv: u16,
    color: u32,
}

#[derive(Clone, Debug)]
struct Surface {
    plane: u16,
    face_flags: u16,
    material: u16,
    template_eligible: bool,
    policy_visible: bool,
    liquid: bool,
    mins: [i16; 3],
    maxs: [i16; 3],
    positions: Vec<u16>,
    corners: Vec<SurfaceCorner>,
    tpage: u16,
}

#[derive(Clone, Debug)]
struct Batch {
    signature: Vec<u64>,
    faces: Vec<usize>,
    positions: Vec<u16>,
}

impl Batch {
    fn first_face(&self) -> usize {
        self.faces[0]
    }

    fn is_contiguous(&self) -> bool {
        self.faces.windows(2).all(|pair| pair[1] == pair[0] + 1)
    }
}

#[derive(Clone, Debug, Default)]
struct ViewMetrics {
    faces: usize,
    base_packet_bytes: usize,
    active_batches: usize,
    active_position_references: usize,
    unique_positions: usize,
    corner_references: usize,
    reordered_faces: usize,
    material_references: usize,
    material_changes: usize,
    ambiguous_facing: usize,
    eligible_packet_bytes: usize,
    invariant_front_packet_bytes: usize,
    invariant_back_packet_bytes: usize,
    ambiguous_template_packet_bytes: usize,
    cell_faces: usize,
    cell_dynamic_facing: usize,
    cell_invariant_front: usize,
    cell_invariant_back_pruned: usize,
    cell_policy_pruned: usize,
    cell_blocks: usize,
    cell_stream_bytes: usize,
}

#[derive(Clone, Debug)]
struct MapCensus {
    map: String,
    faces: usize,
    pvs_faces: usize,
    positions: usize,
    visibility_classes: usize,
    ordered_batches: Vec<Batch>,
    connected_batches: Vec<Batch>,
    surface_packet_bytes: Vec<usize>,
    views: Vec<ViewMetrics>,
    facing_pairs: usize,
    invariant_facing_pairs: usize,
    quad_payload: QuadPayloadMetrics,
    masked_objects: MaskedObjectMetrics,
    resident_bytes: usize,
    resident_core_bytes: usize,
    render_sections: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct QuadPayloadMetrics {
    payload_bytes: usize,
    activation_bytes: usize,
    objects: usize,
    quads: usize,
    positions: usize,
    commands: usize,
    invariant_commands: usize,
    dynamic_commands: usize,
    pruned_back_commands: usize,
    spilled_commands: usize,
    spilled_faces: usize,
    spill_fallback_bytes: usize,
    template_faces: usize,
    excluded_faces: usize,
    baked_overflow_corners: usize,
    odd_fallback_triangles: usize,
    fallback_p50_bytes: usize,
    fallback_p95_bytes: usize,
    fallback_max_bytes: usize,
    leaf_payload_total_bytes: usize,
    leaf_payload_p50_bytes: usize,
    leaf_payload_p95_bytes: usize,
    leaf_payload_max_bytes: usize,
    leaf_activation_p50_bytes: usize,
    leaf_activation_p95_bytes: usize,
    leaf_activation_max_bytes: usize,
    leaf_total_p50_bytes: usize,
    leaf_total_p95_bytes: usize,
    leaf_total_max_bytes: usize,
    section_count: usize,
    section_oversize_count: usize,
    section_payload_total_bytes: usize,
    section_sidecar_bytes: usize,
    section_leaves_p50: usize,
    section_leaves_p95: usize,
    section_leaves_max: usize,
    section_activation_p50_bytes: usize,
    section_activation_p95_bytes: usize,
    section_activation_max_bytes: usize,
    section_gpu_p50_bytes: usize,
    section_gpu_p95_bytes: usize,
    section_gpu_max_bytes: usize,
    section_transition_max_bytes: usize,
}

#[derive(Clone, Debug)]
struct MaskedRenderObject {
    faces: Vec<usize>,
    positions: Vec<u16>,
    quads: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct MaskedObjectMetrics {
    objects: usize,
    faces: usize,
    commands: usize,
    commands_p50: usize,
    commands_p95: usize,
    commands_max: usize,
    selected_quads: usize,
    projected_quads: usize,
    selected_positions: usize,
    projected_positions: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct HotPrefixMetrics {
    resident_bytes: usize,
    resident_batches: usize,
    resident_visit_bytes: usize,
    total_visit_bytes: usize,
    p95_static_high_water: usize,
    maximum_static_high_water: usize,
    minimum_other_headroom: isize,
    overflowing_views: usize,
    nonempty_views: usize,
}

#[derive(Copy, Clone, Debug)]
struct LeafBounds {
    mins: [i16; 3],
    maxs: [i16; 3],
}

#[derive(Clone, Debug)]
struct DisjointSet {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl DisjointSet {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
            rank: vec![0; len],
        }
    }

    fn find(&mut self, value: usize) -> usize {
        let parent = self.parent[value];
        if parent != value {
            self.parent[value] = self.find(parent);
        }
        self.parent[value]
    }

    fn union(&mut self, left: usize, right: usize) {
        let mut left = self.find(left);
        let mut right = self.find(right);
        if left == right {
            return;
        }
        if self.rank[left] < self.rank[right] {
            std::mem::swap(&mut left, &mut right);
        }
        self.parent[right] = left;
        if self.rank[left] == self.rank[right] {
            self.rank[left] += 1;
        }
    }
}

fn signature_has(signature: &[u64], bit: usize) -> bool {
    signature
        .get(bit >> 6)
        .is_some_and(|word| word & (1u64 << (bit & 63)) != 0)
}

fn signature_is_empty(signature: &[u64]) -> bool {
    signature.iter().all(|word| *word == 0)
}

fn push_batch(
    output: &mut Vec<Batch>,
    signature: &[u64],
    faces: &mut Vec<usize>,
    positions: &mut BTreeSet<u16>,
) {
    if faces.is_empty() {
        return;
    }
    output.push(Batch {
        signature: signature.to_vec(),
        faces: std::mem::take(faces),
        positions: positions.iter().copied().collect(),
    });
    positions.clear();
}

fn append_face_to_bounded_batches(
    output: &mut Vec<Batch>,
    signature: &[u64],
    surfaces: &[Surface],
    faces: &mut Vec<usize>,
    positions: &mut BTreeSet<u16>,
    face_index: usize,
) {
    let surface = &surfaces[face_index];
    let added_positions = surface
        .positions
        .iter()
        .filter(|position| !positions.contains(position))
        .count();
    if !faces.is_empty()
        && (faces.len() == RETAIL_FACE_LIMIT
            || positions.len() + added_positions > RETAIL_VERTEX_LIMIT)
    {
        push_batch(output, signature, faces, positions);
    }
    faces.push(face_index);
    positions.extend(surface.positions.iter().copied());
}

/// Preserve the current global source order.  A new batch begins whenever an
/// exact visibility signature changes, even if a matching signature appears
/// again later.
fn ordered_batches(surfaces: &[Surface], signatures: &[Vec<u64>]) -> Vec<Batch> {
    let mut output = Vec::new();
    let mut faces = Vec::new();
    let mut positions = BTreeSet::new();
    let mut current_signature: Option<&[u64]> = None;
    for face_index in 0..surfaces.len() {
        let signature = &signatures[face_index];
        if signature_is_empty(signature) {
            continue;
        }
        if current_signature.is_some_and(|current| current != signature.as_slice()) {
            push_batch(
                &mut output,
                current_signature.expect("non-empty ordered run"),
                &mut faces,
                &mut positions,
            );
        }
        current_signature = Some(signature);
        append_face_to_bounded_batches(
            &mut output,
            signature,
            surfaces,
            &mut faces,
            &mut positions,
            face_index,
        );
    }
    if let Some(signature) = current_signature {
        push_batch(&mut output, signature, &mut faces, &mut positions);
    }
    output
}

/// Recover brush-like connected components inside one exact visibility class.
/// Faces connect only across a complete shared boundary edge, not at a single
/// corner.  Components are then split to the retail 32-face/255-position caps.
fn connected_batches(surfaces: &[Surface], signatures: &[Vec<u64>]) -> Vec<Batch> {
    let mut classes = BTreeMap::<Vec<u64>, Vec<usize>>::new();
    for (face_index, signature) in signatures.iter().enumerate() {
        if !signature_is_empty(signature) {
            classes
                .entry(signature.clone())
                .or_default()
                .push(face_index);
        }
    }

    let mut output = Vec::new();
    for (signature, class_faces) in classes {
        let mut disjoint = DisjointSet::new(class_faces.len());
        let mut edge_owner = BTreeMap::<(u16, u16), usize>::new();
        for (local_index, &face_index) in class_faces.iter().enumerate() {
            let positions = &surfaces[face_index].positions;
            for edge in positions
                .iter()
                .copied()
                .zip(positions.iter().copied().cycle().skip(1))
                .take(positions.len())
            {
                let key = if edge.0 <= edge.1 {
                    edge
                } else {
                    (edge.1, edge.0)
                };
                if let Some(&other) = edge_owner.get(&key) {
                    disjoint.union(local_index, other);
                } else {
                    edge_owner.insert(key, local_index);
                }
            }
        }

        let mut components = BTreeMap::<usize, Vec<usize>>::new();
        for (local_index, &face_index) in class_faces.iter().enumerate() {
            let root = disjoint.find(local_index);
            components.entry(root).or_default().push(face_index);
        }
        let mut components = components.into_values().collect::<Vec<_>>();
        for component in &mut components {
            component.sort_unstable();
        }
        components.sort_by_key(|component| component[0]);

        for component in components {
            let mut faces = Vec::new();
            let mut positions = BTreeSet::new();
            for face_index in component {
                append_face_to_bounded_batches(
                    &mut output,
                    &signature,
                    surfaces,
                    &mut faces,
                    &mut positions,
                    face_index,
                );
            }
            push_batch(&mut output, &signature, &mut faces, &mut positions);
        }
    }
    output.sort_by_key(Batch::first_face);
    output
}

fn material_changes(order: &[usize], surfaces: &[Surface]) -> usize {
    let mut previous = None;
    let mut changes = 0usize;
    for &face in order {
        let material = surfaces[face].material;
        if previous != Some(material) {
            changes += 1;
            previous = Some(material);
        }
    }
    changes
}

fn surface_packet_bytes(surface: &Surface) -> usize {
    let root_triangles = surface.positions.len().saturating_sub(2);
    (root_triangles / 2) * 52 + (root_triangles & 1) * 40
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellFace {
    face: u16,
    dynamic_facing: bool,
}

const CELL_STREAM_BLOCK_FACES: usize = 16;
const CELL_STREAM_BLOCK_HEADER_BYTES: usize = 16;
const CELL_STREAM_ESCAPE: u8 = 0x7f;

/// Encode one retained camera-cell stream. Each conservative block begins
/// with a byte skip length, face count, and exact i16 union bounds. Face
/// references use a one-byte source-index delta when possible; bit seven
/// distinguishes leaf-invariant fronts from faces which still need the
/// authoritative runtime plane test. Escape records carry a u16 absolute
/// source face. This is a destination prototype, not a claimed retail format.
fn encode_cell_stream(entries: &[CellFace], surfaces: &[Surface]) -> Vec<u8> {
    let mut output = Vec::new();
    for block in entries.chunks(CELL_STREAM_BLOCK_FACES) {
        let mut mins = surfaces[block[0].face as usize].mins;
        let mut maxs = surfaces[block[0].face as usize].maxs;
        for entry in &block[1..] {
            let surface = &surfaces[entry.face as usize];
            for axis in 0..3 {
                mins[axis] = mins[axis].min(surface.mins[axis]);
                maxs[axis] = maxs[axis].max(surface.maxs[axis]);
            }
        }
        let header = output.len();
        output.extend_from_slice(&[0, 0, block.len() as u8, 0]);
        for value in mins.into_iter().chain(maxs) {
            output.extend_from_slice(&value.to_le_bytes());
        }
        debug_assert_eq!(output.len() - header, CELL_STREAM_BLOCK_HEADER_BYTES);

        let payload = output.len();
        let mut previous = 0u16;
        for (index, entry) in block.iter().enumerate() {
            let delta = entry.face.wrapping_sub(previous);
            let mode = u8::from(entry.dynamic_facing) << 7;
            if delta < u16::from(CELL_STREAM_ESCAPE) {
                output.push(mode | delta as u8);
            } else {
                output.push(mode | CELL_STREAM_ESCAPE);
                output.extend_from_slice(&entry.face.to_le_bytes());
            }
            previous = entry.face;
            debug_assert!(index == 0 || block[index - 1].face < entry.face);
        }
        let payload_bytes = u16::try_from(output.len() - payload).expect("bounded block payload");
        output[header..header + 2].copy_from_slice(&payload_bytes.to_le_bytes());
    }
    output
}

fn decode_cell_stream(bytes: &[u8]) -> Option<Vec<CellFace>> {
    let mut output = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let header = bytes.get(cursor..cursor + CELL_STREAM_BLOCK_HEADER_BYTES)?;
        let payload_bytes = u16::from_le_bytes([header[0], header[1]]) as usize;
        let face_count = header[2] as usize;
        cursor += CELL_STREAM_BLOCK_HEADER_BYTES;
        let end = cursor.checked_add(payload_bytes)?;
        let payload = bytes.get(cursor..end)?;
        let mut payload_cursor = 0usize;
        let mut previous = 0u16;
        for _ in 0..face_count {
            let code = *payload.get(payload_cursor)?;
            payload_cursor += 1;
            let value = code & 0x7f;
            let face = if value == CELL_STREAM_ESCAPE {
                let absolute = payload.get(payload_cursor..payload_cursor + 2)?;
                payload_cursor += 2;
                u16::from_le_bytes([absolute[0], absolute[1]])
            } else {
                previous.checked_add(u16::from(value))?
            };
            output.push(CellFace {
                face,
                dynamic_facing: code & 0x80 != 0,
            });
            previous = face;
        }
        if payload_cursor != payload.len() {
            return None;
        }
        cursor = end;
    }
    Some(output)
}

fn cell_stream_metrics(
    faces: &[usize],
    surfaces: &[Surface],
    planes: &[Plane],
    bounds: LeafBounds,
) -> Result<(Vec<CellFace>, ViewMetrics)> {
    let mut entries = Vec::new();
    let mut metrics = ViewMetrics::default();
    for &face_index in faces {
        let surface = &surfaces[face_index];
        if !surface.policy_visible {
            metrics.cell_policy_pruned += 1;
            continue;
        }
        let invariant = if surface.liquid {
            // A water portal can override ordinary facing for its exact plane.
            // Retaining all liquid faces keeps this host bound conservative.
            None
        } else {
            let plane = *planes.get(surface.plane as usize).ok_or_else(|| {
                format!("cell stream face plane {} is out of range", surface.plane)
            })?;
            leaf_invariant_facing(plane, surface.face_flags, bounds)
        };
        match invariant {
            Some(false) => metrics.cell_invariant_back_pruned += 1,
            Some(true) => {
                metrics.cell_invariant_front += 1;
                entries.push(CellFace {
                    face: face_index as u16,
                    dynamic_facing: false,
                });
            }
            None => {
                metrics.cell_dynamic_facing += 1;
                entries.push(CellFace {
                    face: face_index as u16,
                    dynamic_facing: true,
                });
            }
        }
    }
    let stream = encode_cell_stream(&entries, surfaces);
    if decode_cell_stream(&stream).as_deref() != Some(entries.as_slice()) {
        return Err("cell stream failed exact encode/decode roundtrip".into());
    }
    metrics.cell_faces = entries.len();
    metrics.cell_blocks = entries.len().div_ceil(CELL_STREAM_BLOCK_FACES);
    metrics.cell_stream_bytes = stream.len();
    Ok((entries, metrics))
}

fn view_metrics(
    view_index: usize,
    view_faces: &[usize],
    surfaces: &[Surface],
    batches: &[Batch],
) -> ViewMetrics {
    if view_faces.is_empty() {
        return ViewMetrics::default();
    }
    let active = batches
        .iter()
        .filter(|batch| signature_has(&batch.signature, view_index))
        .collect::<Vec<_>>();
    let mut batched_order = Vec::with_capacity(view_faces.len());
    let mut active_position_references = 0usize;
    for batch in &active {
        active_position_references += batch.positions.len();
        batched_order.extend(batch.faces.iter().copied());
    }
    let reordered_faces = batched_order
        .iter()
        .zip(view_faces)
        .filter(|(batched, source)| batched != source)
        .count()
        + batched_order.len().abs_diff(view_faces.len());
    let unique_positions = view_faces
        .iter()
        .flat_map(|&face| surfaces[face].positions.iter().copied())
        .collect::<BTreeSet<_>>()
        .len();
    let corner_references = view_faces
        .iter()
        .map(|&face| surfaces[face].positions.len())
        .sum();
    // PSoXide's compact classic packets are byte-identical in size to the
    // retail renderer's resident GT3/GT4 shapes: 40 bytes per textured
    // Gouraud triangle and 52 per quad, tag included. Adjacent fan triangles
    // already pair into a quad in the no-subdivision path.
    let base_packet_bytes = view_faces
        .iter()
        .map(|&face| surface_packet_bytes(&surfaces[face]))
        .sum();
    ViewMetrics {
        faces: view_faces.len(),
        base_packet_bytes,
        active_batches: active.len(),
        active_position_references,
        unique_positions,
        corner_references,
        reordered_faces,
        material_references: batched_order.len(),
        material_changes: material_changes(&batched_order, surfaces),
        ambiguous_facing: 0,
        ..ViewMetrics::default()
    }
}

fn hot_prefix_metrics(
    batches: &[Batch],
    surface_packet_bytes: &[usize],
    views: &[ViewMetrics],
    prefix_budget: usize,
) -> HotPrefixMetrics {
    let batch_bytes = batches
        .iter()
        .map(|batch| {
            batch
                .faces
                .iter()
                .map(|&face| surface_packet_bytes[face])
                .sum::<usize>()
        })
        .collect::<Vec<_>>();
    let active_views = batches
        .iter()
        .map(|batch| {
            batch
                .signature
                .iter()
                .map(|word| word.count_ones() as usize)
                .sum::<usize>()
        })
        .collect::<Vec<_>>();
    let mut priority = (0..batches.len()).collect::<Vec<_>>();
    priority.sort_unstable_by_key(|&index| {
        (
            std::cmp::Reverse(active_views[index]),
            batch_bytes[index],
            batches[index].first_face(),
        )
    });

    let mut resident = vec![false; batches.len()];
    let mut metrics = HotPrefixMetrics::default();
    for index in priority {
        let Some(end) = metrics.resident_bytes.checked_add(batch_bytes[index]) else {
            continue;
        };
        if end > prefix_budget {
            continue;
        }
        resident[index] = true;
        metrics.resident_bytes = end;
        metrics.resident_batches += 1;
    }

    let usable_arena = GPU_ARENA_BYTES - GPU_ARENA_SAFETY_BYTES;
    let mut high_waters = Vec::new();
    for (view_index, view) in views.iter().enumerate() {
        if view.faces == 0 {
            continue;
        }
        metrics.nonempty_views += 1;
        metrics.total_visit_bytes += view.base_packet_bytes;
        let resident_selected = batches
            .iter()
            .enumerate()
            .filter(|(index, batch)| {
                resident[*index] && signature_has(&batch.signature, view_index)
            })
            .map(|(index, _)| batch_bytes[index])
            .sum::<usize>();
        metrics.resident_visit_bytes += resident_selected;
        let high_water = metrics
            .resident_bytes
            .saturating_add(view.base_packet_bytes.saturating_sub(resident_selected));
        high_waters.push(high_water);
        if high_water > usable_arena {
            metrics.overflowing_views += 1;
        }
    }
    metrics.p95_static_high_water = percentile(high_waters.iter().copied(), 95);
    metrics.maximum_static_high_water = high_waters.iter().copied().max().unwrap_or(0);
    metrics.minimum_other_headroom = isize::try_from(usable_arena).unwrap_or(isize::MAX)
        - isize::try_from(metrics.maximum_static_high_water).unwrap_or(isize::MAX);
    metrics
}

fn percentile(values: impl IntoIterator<Item = usize>, percentile: usize) -> usize {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let index = (values.len() - 1) * percentile / 100;
    values[index]
}

fn packed_lump_bytes(index: &PsbIndex, lumps: &[LumpKind]) -> Option<usize> {
    lumps.iter().try_fold(0usize, |bytes, &kind| {
        bytes
            .checked_add(3)
            .map(|bytes| bytes & !3)
            .and_then(|bytes| bytes.checked_add(index.lump(kind).len as usize))
    })
}

fn resident_and_collision_core_bytes(index: &PsbIndex) -> Result<(usize, usize)> {
    const RESIDENT: [LumpKind; 13] = [
        LumpKind::ModelData,
        LumpKind::Vertices,
        LumpKind::Planes,
        LumpKind::TextureInfo,
        LumpKind::Faces,
        LumpKind::MarkSurfaces,
        LumpKind::Visibility,
        LumpKind::Leaves,
        LumpKind::Nodes,
        LumpKind::ClipNodes,
        LumpKind::Models,
        LumpKind::Strings,
        LumpKind::Entities,
    ];
    // Vertices, texture info, faces, mark surfaces, and PVS are replaced by
    // the streamed render section. Nodes and leaves remain because collision,
    // point-leaf queries, entity visibility, and submodel ownership use them.
    const CORE: [LumpKind; 8] = [
        LumpKind::ModelData,
        LumpKind::Planes,
        LumpKind::Leaves,
        LumpKind::Nodes,
        LumpKind::ClipNodes,
        LumpKind::Models,
        LumpKind::Strings,
        LumpKind::Entities,
    ];
    Ok((
        packed_lump_bytes(index, &RESIDENT).ok_or("resident lump byte count overflow")?,
        packed_lump_bytes(index, &CORE).ok_or("resident core byte count overflow")?,
    ))
}

fn source_leaf_bounds(bsp: &Bsp<'_>) -> Vec<LeafBounds> {
    bsp.lump(BspLump::Leaves)
        .chunks_exact(28)
        .map(|record| {
            let value = |offset: usize| i16::from_le_bytes([record[offset], record[offset + 1]]);
            LeafBounds {
                mins: [value(8), value(10), value(12)]
                    .map(|value| decode_leaf_bound_min(encode_leaf_bound_min(value))),
                maxs: [value(14), value(16), value(18)]
                    .map(|value| decode_leaf_bound_max(encode_leaf_bound_max(value))),
            }
        })
        .collect()
}

/// A Quake leaf is wholly contained by its authored AABB. If the supporting
/// plane does not cross that AABB, its facing result cannot change anywhere
/// inside the leaf and may be decided once on a leaf transition.
fn facing_is_leaf_invariant(plane: Plane, bounds: LeafBounds) -> bool {
    let normal = [plane.normal.x, plane.normal.y, plane.normal.z];
    let mut minimum = 0i64;
    let mut maximum = 0i64;
    for axis in 0..3 {
        let (near, far) = if normal[axis] < 0 {
            (bounds.maxs[axis], bounds.mins[axis])
        } else {
            (bounds.mins[axis], bounds.maxs[axis])
        };
        minimum += i64::from(near) * i64::from(normal[axis]);
        maximum += i64::from(far) * i64::from(normal[axis]);
    }
    minimum -= i64::from(plane.distance);
    maximum -= i64::from(plane.distance);
    minimum >= 0 || maximum < 0
}

fn leaf_invariant_facing(plane: Plane, face_flags: u16, bounds: LeafBounds) -> Option<bool> {
    let normal = [plane.normal.x, plane.normal.y, plane.normal.z];
    let mut minimum = 0i64;
    let mut maximum = 0i64;
    for axis in 0..3 {
        let (near, far) = if normal[axis] < 0 {
            (bounds.maxs[axis], bounds.mins[axis])
        } else {
            (bounds.mins[axis], bounds.maxs[axis])
        };
        minimum += i64::from(near) * i64::from(normal[axis]);
        maximum += i64::from(far) * i64::from(normal[axis]);
    }
    minimum -= i64::from(plane.distance);
    maximum -= i64::from(plane.distance);
    let behind = if maximum < 0 {
        true
    } else if minimum >= 0 {
        false
    } else {
        return None;
    };
    Some(behind == (face_flags & FACE_BACKSIDE != 0))
}

fn leaf_local_quad_memory(
    input: &RenderQuadPayloadInput,
    commands: &[RenderQuadCommand],
) -> Result<(usize, usize, usize)> {
    let mut face_count = 0usize;
    let mut quad_count = 0usize;
    let mut position_count = 0usize;
    let mut run_count = 0usize;
    for command in commands {
        let object = input
            .objects
            .get(command.object as usize)
            .ok_or("leaf-local QRP3 command references a missing object")?;
        face_count = face_count
            .checked_add(object.face_count as usize)
            .ok_or("leaf-local QRP3 face count overflow")?;
        quad_count = quad_count
            .checked_add(object.quad_count as usize)
            .ok_or("leaf-local QRP3 quad count overflow")?;
        position_count = position_count
            .checked_add(object.position_count as usize)
            .ok_or("leaf-local QRP3 position count overflow")?;
        run_count = run_count
            .checked_add(object.run_count as usize)
            .ok_or("leaf-local QRP3 run count overflow")?;
    }
    let object_count = commands.len();
    let objects_end = RENDER_QUAD_HEADER_BYTES
        .checked_add(object_count * RENDER_QUAD_OBJECT_BYTES)
        .ok_or("leaf-local QRP3 payload overflow")?;
    let faces_end = objects_end
        .checked_add(face_count * RENDER_QUAD_FACE_BYTES)
        .ok_or("leaf-local QRP3 payload overflow")?;
    let quads_end = faces_end
        .checked_add(quad_count * RENDER_QUAD_RECORD_BYTES)
        .ok_or("leaf-local QRP3 payload overflow")?;
    let positions_end = quads_end
        .checked_add(position_count * RENDER_QUAD_POSITION_BYTES)
        .ok_or("leaf-local QRP3 payload overflow")?;
    let runs_offset = (positions_end + 3) & !3;
    let payload_bytes = runs_offset
        .checked_add(run_count * RENDER_QUAD_RUN_BYTES)
        .and_then(|bytes| bytes.checked_add(12))
        .and_then(|bytes| bytes.checked_add(object_count * RENDER_QUAD_COMMAND_BYTES))
        .ok_or("leaf-local QRP3 payload overflow")?;
    let runtime_metadata_bytes = object_count
        .checked_mul(RENDER_QUAD_OBJECT_BYTES)
        .and_then(|bytes| bytes.checked_add(face_count * RENDER_QUAD_FACE_BYTES))
        .and_then(|bytes| bytes.checked_add(quad_count * RENDER_QUAD_REFERENCE_BYTES))
        .and_then(|bytes| bytes.checked_add(position_count * RENDER_QUAD_POSITION_BYTES))
        .and_then(|bytes| bytes.checked_add(12))
        .and_then(|bytes| bytes.checked_add(object_count * RENDER_QUAD_COMMAND_BYTES))
        .ok_or("leaf-local QRP3 runtime metadata overflow")?;
    let activation_bytes = runtime_metadata_bytes
        .checked_add(position_count * RENDER_QUAD_PROJECTED_POSITION_BYTES)
        .ok_or("leaf-local QRP3 activation overflow")?;
    let packet_pool_bytes = quad_count
        .checked_mul(RENDER_QUAD_PACKET_BYTES)
        .ok_or("leaf-local QRP3 packet-pool overflow")?;
    Ok((payload_bytes, activation_bytes, packet_pool_bytes))
}

fn section_quad_memory(
    input: &RenderQuadPayloadInput,
    first_cell: usize,
    end_cell: usize,
    fallback_bytes: &[usize],
) -> Result<(usize, usize, usize)> {
    let cells = input
        .cells
        .get(first_cell..end_cell)
        .ok_or("QRP3 section cell range is invalid")?;
    let mut object_indices = BTreeSet::new();
    let mut command_count = 0usize;
    for cell in cells {
        command_count = command_count
            .checked_add(cell.commands.len())
            .ok_or("QRP3 section command count overflow")?;
        object_indices.extend(cell.commands.iter().map(|command| command.object));
    }
    let mut face_count = 0usize;
    let mut quad_count = 0usize;
    let mut position_count = 0usize;
    let mut run_count = 0usize;
    for object_index in &object_indices {
        let object = input
            .objects
            .get(*object_index as usize)
            .ok_or("QRP3 section references a missing object")?;
        face_count = face_count
            .checked_add(object.face_count as usize)
            .ok_or("QRP3 section face count overflow")?;
        quad_count = quad_count
            .checked_add(object.quad_count as usize)
            .ok_or("QRP3 section quad count overflow")?;
        position_count = position_count
            .checked_add(object.position_count as usize)
            .ok_or("QRP3 section position count overflow")?;
        run_count = run_count
            .checked_add(object.run_count as usize)
            .ok_or("QRP3 section run count overflow")?;
    }
    let object_count = object_indices.len();
    let objects_bytes = object_count
        .checked_mul(RENDER_QUAD_OBJECT_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let faces_bytes = face_count
        .checked_mul(RENDER_QUAD_FACE_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let quads_bytes = quad_count
        .checked_mul(RENDER_QUAD_RECORD_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let positions_bytes = position_count
        .checked_mul(RENDER_QUAD_POSITION_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let runs_bytes = run_count
        .checked_mul(RENDER_QUAD_RUN_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let cells_bytes = cells
        .len()
        .checked_mul(12)
        .ok_or("QRP3 section payload overflow")?;
    let commands_bytes = command_count
        .checked_mul(RENDER_QUAD_COMMAND_BYTES)
        .ok_or("QRP3 section payload overflow")?;
    let positions_end = RENDER_QUAD_HEADER_BYTES
        .checked_add(objects_bytes)
        .and_then(|bytes| bytes.checked_add(faces_bytes))
        .and_then(|bytes| bytes.checked_add(quads_bytes))
        .and_then(|bytes| bytes.checked_add(positions_bytes))
        .ok_or("QRP3 section payload overflow")?;
    let payload_bytes = ((positions_end + 3) & !3)
        .checked_add(runs_bytes)
        .and_then(|bytes| bytes.checked_add(cells_bytes))
        .and_then(|bytes| bytes.checked_add(commands_bytes))
        .ok_or("QRP3 section payload overflow")?;
    let runtime_metadata_bytes = objects_bytes
        .checked_add(faces_bytes)
        .and_then(|bytes| bytes.checked_add(quad_count * RENDER_QUAD_REFERENCE_BYTES))
        .and_then(|bytes| bytes.checked_add(positions_bytes))
        .and_then(|bytes| bytes.checked_add(cells_bytes))
        .and_then(|bytes| bytes.checked_add(commands_bytes))
        .ok_or("QRP3 section runtime metadata overflow")?;
    let packet_pool_bytes = quad_count
        .checked_mul(RENDER_QUAD_PACKET_BYTES)
        .ok_or("QRP3 section packet-pool overflow")?;
    let projection_bytes = position_count
        .checked_mul(RENDER_QUAD_PROJECTED_POSITION_BYTES)
        .ok_or("QRP3 section projection overflow")?;
    let fallback = fallback_bytes
        .get(first_cell..end_cell)
        .ok_or("QRP3 section fallback range is invalid")?
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let activation_bytes = runtime_metadata_bytes
        .checked_add(projection_bytes)
        .ok_or("QRP3 section activation overflow")?;
    let gpu_bytes = packet_pool_bytes
        .checked_add(fallback)
        .ok_or("QRP3 section GPU high-water overflow")?;
    Ok((payload_bytes, activation_bytes, gpu_bytes))
}

fn encode_local_quad_section(
    source: &RenderQuadPayloadInput,
    first_cell: usize,
    end_cell: usize,
) -> Result<quake_cook::EncodedRenderQuadPayload> {
    let source_cells = source
        .cells
        .get(first_cell..end_cell)
        .ok_or("QRP3 section cell range is invalid")?;
    let object_indices = source_cells
        .iter()
        .flat_map(|cell| cell.commands.iter().map(|command| command.object))
        .collect::<BTreeSet<_>>();
    let mut output = RenderQuadPayloadInput::default();
    let mut object_remap = BTreeMap::<u16, u16>::new();

    for source_object_index in object_indices {
        let source_object = *source
            .objects
            .get(source_object_index as usize)
            .ok_or("QRP3 section references a missing object")?;
        let local_object_index = u16::try_from(output.objects.len())
            .map_err(|_| "QRP3 section object count exceeds u16")?;
        object_remap.insert(source_object_index, local_object_index);
        let first_face =
            u16::try_from(output.faces.len()).map_err(|_| "QRP3 section face count exceeds u16")?;
        let first_quad =
            u16::try_from(output.quads.len()).map_err(|_| "QRP3 section quad count exceeds u16")?;
        let first_position = u16::try_from(output.positions.len())
            .map_err(|_| "QRP3 section position count exceeds u16")?;
        let first_run =
            u16::try_from(output.runs.len()).map_err(|_| "QRP3 section run count exceeds u16")?;

        for source_face_index in source_object.first_face as usize
            ..source_object.first_face as usize + source_object.face_count as usize
        {
            let mut face = *source
                .faces
                .get(source_face_index)
                .ok_or("QRP3 section object has a bad face range")?;
            let relative = face
                .first_quad
                .checked_sub(source_object.first_quad)
                .ok_or("QRP3 section face begins before its object")?;
            face.first_quad = first_quad
                .checked_add(relative)
                .ok_or("QRP3 section face range exceeds u16")?;
            output.faces.push(face);
        }
        for source_quad_index in source_object.first_quad as usize
            ..source_object.first_quad as usize + source_object.quad_count as usize
        {
            output.quads.push(
                *source
                    .quads
                    .get(source_quad_index)
                    .ok_or("QRP3 section object has a bad quad range")?,
            );
        }
        output.positions.extend_from_slice(
            source
                .positions
                .get(
                    source_object.first_position as usize
                        ..source_object.first_position as usize
                            + source_object.position_count as usize,
                )
                .ok_or("QRP3 section object has a bad position range")?,
        );
        for source_run_index in source_object.first_run as usize
            ..source_object.first_run as usize + source_object.run_count as usize
        {
            let mut run = *source
                .runs
                .get(source_run_index)
                .ok_or("QRP3 section object has a bad run range")?;
            let relative = run
                .first_quad
                .checked_sub(source_object.first_quad)
                .ok_or("QRP3 section run begins before its object")?;
            run.first_quad = first_quad
                .checked_add(relative)
                .ok_or("QRP3 section run range exceeds u16")?;
            output.runs.push(run);
        }
        output.objects.push(RenderQuadObject {
            first_face,
            first_quad,
            first_position,
            first_run,
            ..source_object
        });
    }

    for source_cell in source_cells {
        let commands = source_cell
            .commands
            .iter()
            .map(|command| {
                Ok(RenderQuadCommand {
                    object: *object_remap
                        .get(&command.object)
                        .ok_or("QRP3 section command object was not localized")?,
                    flags: command.flags,
                    visible_faces: command.visible_faces,
                    dynamic_faces: command.dynamic_faces,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        output.cells.push(RenderQuadCellInput {
            leaf: source_cell.leaf,
            flags: source_cell.flags,
            commands,
        });
    }
    Ok(encode_render_quad_payload(&output)?)
}

fn push_masked_object(
    output: &mut Vec<MaskedRenderObject>,
    faces: &mut Vec<usize>,
    positions: &mut BTreeSet<u16>,
    quads: &mut usize,
) {
    if faces.is_empty() {
        return;
    }
    output.push(MaskedRenderObject {
        faces: std::mem::take(faces),
        positions: positions.iter().copied().collect(),
        quads: *quads,
    });
    positions.clear();
    *quads = 0;
}

fn masked_render_objects(
    surfaces: &[Surface],
    signatures: &[Vec<u64>],
) -> Result<Vec<MaskedRenderObject>> {
    let mut objects = Vec::new();
    let mut faces = Vec::new();
    let mut positions = BTreeSet::new();
    let mut quads = 0usize;
    for (face_index, surface) in surfaces.iter().enumerate() {
        let face_quads = surface.corners.len().saturating_sub(2) / 2;
        if !surface.policy_visible
            || !surface.template_eligible
            || signature_is_empty(&signatures[face_index])
            || face_quads == 0
        {
            push_masked_object(&mut objects, &mut faces, &mut positions, &mut quads);
            continue;
        }
        if face_quads > RENDER_QUAD_OBJECT_MAX_QUADS {
            return Err(format!("face {face_index} exceeds the masked-object quad ceiling").into());
        }
        let added_positions = surface
            .positions
            .iter()
            .filter(|position| !positions.contains(position))
            .count();
        if !faces.is_empty()
            && (faces.len() == RENDER_QUAD_OBJECT_MAX_FACES
                || quads + face_quads > RENDER_QUAD_OBJECT_MAX_QUADS
                || positions.len() + added_positions > RENDER_QUAD_OBJECT_MAX_POSITIONS)
        {
            push_masked_object(&mut objects, &mut faces, &mut positions, &mut quads);
        }
        faces.push(face_index);
        positions.extend(surface.positions.iter().copied());
        quads += face_quads;
    }
    push_masked_object(&mut objects, &mut faces, &mut positions, &mut quads);
    Ok(objects)
}

/// Measure an exact source-order object alternative. Consecutive eligible
/// faces share one object even when their PVS/facing signatures differ; a
/// runtime command would carry per-face visible and dynamic masks. This keeps
/// ordering stable while testing whether coarser commands save enough work to
/// justify projecting masked-out members of an admitted object.
fn masked_object_metrics(
    surfaces: &[Surface],
    signatures: &[Vec<u64>],
    view_faces: &[Vec<usize>],
    planes: &[Plane],
    source_bounds: &[LeafBounds],
) -> Result<MaskedObjectMetrics> {
    let objects = masked_render_objects(surfaces, signatures)?;

    let mut metrics = MaskedObjectMetrics {
        objects: objects.len(),
        faces: objects.iter().map(|object| object.faces.len()).sum(),
        ..MaskedObjectMetrics::default()
    };
    let mut commands_per_view = Vec::new();
    for (view_index, visible_faces) in view_faces.iter().enumerate() {
        if visible_faces.is_empty() {
            continue;
        }
        let bounds = source_bounds[view_index + 1];
        let mut view_commands = 0usize;
        for object in &objects {
            let mut selected_quads = 0usize;
            let mut selected_positions = BTreeSet::new();
            for &face_index in &object.faces {
                if !signature_has(&signatures[face_index], view_index) {
                    continue;
                }
                let surface = &surfaces[face_index];
                let plane = *planes
                    .get(surface.plane as usize)
                    .ok_or("masked object face plane is out of range")?;
                if leaf_invariant_facing(plane, surface.face_flags, bounds) == Some(false) {
                    continue;
                }
                selected_quads += surface.corners.len().saturating_sub(2) / 2;
                selected_positions.extend(surface.positions.iter().copied());
            }
            if selected_quads == 0 {
                continue;
            }
            view_commands += 1;
            metrics.selected_quads += selected_quads;
            metrics.projected_quads += object.quads;
            metrics.selected_positions += selected_positions.len();
            metrics.projected_positions += object.positions.len();
        }
        metrics.commands += view_commands;
        commands_per_view.push(view_commands);
    }
    metrics.commands_p50 = percentile(commands_per_view.iter().copied(), 50);
    metrics.commands_p95 = percentile(commands_per_view.iter().copied(), 95);
    metrics.commands_max = commands_per_view.into_iter().max().unwrap_or(0);
    Ok(metrics)
}

/// Compile the real cooked map into the exact base-level subset QRP3 can own.
///
/// Consecutive eligible faces share one bounded object and cell commands carry
/// exact visible/dynamic-facing masks. Object-local u8 position indices feed a
/// sequential fixed kernel. Adjacent fan triangles retain the live renderer's
/// `[previous,current,root,next]` GT4 order. Source face identities allow the
/// accelerated stream to merge with odd-triangle, animated/special-material,
/// near, and adaptive-subdivision fallback work without reordering the OT.
fn build_quad_payload(
    surfaces: &[Surface],
    signatures: &[Vec<u64>],
    view_faces: &[Vec<usize>],
    planes: &[Plane],
    source_bounds: &[LeafBounds],
    source_positions: &[[i16; 3]],
    resident_core_bytes: usize,
) -> Result<(QuadPayloadMetrics, Vec<u8>)> {
    let mut input = RenderQuadPayloadInput::default();
    let mut metrics = QuadPayloadMetrics::default();
    let mut leaf_payload_bytes = Vec::with_capacity(view_faces.len());
    let mut leaf_activation_bytes = Vec::with_capacity(view_faces.len());
    let mut leaf_packet_pool_bytes = Vec::with_capacity(view_faces.len());
    let objects = masked_render_objects(surfaces, signatures)?;

    for (face_index, surface) in surfaces.iter().enumerate() {
        if signature_is_empty(&signatures[face_index]) {
            continue;
        }
        let root_triangles = surface.corners.len().saturating_sub(2);
        let quad_count = root_triangles / 2;
        if !surface.policy_visible || !surface.template_eligible || quad_count == 0 {
            if surface.policy_visible {
                metrics.excluded_faces += 1;
            }
            continue;
        }
        metrics.template_faces += 1;
        metrics.odd_fallback_triangles += root_triangles & 1;
        metrics.baked_overflow_corners += surface
            .corners
            .iter()
            .filter(|corner| corner.color & 0xff00_0000 != 0)
            .count();
    }

    for object_faces in &objects {
        let first_face =
            u16::try_from(input.faces.len()).map_err(|_| "render face count exceeds QRP3")?;
        let first_quad =
            u16::try_from(input.quads.len()).map_err(|_| "quad template count exceeds QRP3")?;
        let first_position =
            u16::try_from(input.positions.len()).map_err(|_| "quad position count exceeds QRP3")?;
        let first_run =
            u16::try_from(input.runs.len()).map_err(|_| "quad material-run count exceeds QRP3")?;
        let mut local_indices = BTreeMap::<u16, u8>::new();
        let mut local_positions = Vec::<[i16; 3]>::new();
        let mut mins = [i16::MAX; 3];
        let mut maxs = [i16::MIN; 3];

        for &face_index in &object_faces.faces {
            let surface = &surfaces[face_index];
            for axis in 0..3 {
                mins[axis] = mins[axis].min(surface.mins[axis]);
                maxs[axis] = maxs[axis].max(surface.maxs[axis]);
            }
            let face_first_quad =
                u16::try_from(input.quads.len()).map_err(|_| "quad template count exceeds QRP3")?;
            let face_quad_count = surface.corners.len().saturating_sub(2) / 2;
            for pair in 0..face_quad_count {
                let previous = 1 + pair * 2;
                let current = previous + 1;
                let next = current + 1;
                let source_corners = [
                    &surface.corners[previous],
                    &surface.corners[current],
                    &surface.corners[0],
                    &surface.corners[next],
                ];
                let mut positions = [0u8; 4];
                for (destination, corner) in positions.iter_mut().zip(source_corners) {
                    *destination = if let Some(&local) = local_indices.get(&corner.position) {
                        local
                    } else {
                        let value = *source_positions
                            .get(corner.position as usize)
                            .ok_or("quad template references a missing source position")?;
                        let local = u8::try_from(local_positions.len())
                            .map_err(|_| "QRP3 object position count exceeds u8")?;
                        local_positions.push(value);
                        local_indices.insert(corner.position, local);
                        local
                    };
                }
                input.quads.push(RenderQuad {
                    positions,
                    invariant_words: [
                        0x3c00_0000 | source_corners[0].color,
                        u32::from(source_corners[0].uv),
                        source_corners[1].color,
                        u32::from(source_corners[1].uv) | (u32::from(surface.tpage) << 16),
                        source_corners[2].color,
                        u32::from(source_corners[2].uv),
                        source_corners[3].color,
                        u32::from(source_corners[3].uv),
                    ],
                });
            }
            let face_quad_count = u16::try_from(face_quad_count).unwrap();
            input.faces.push(RenderQuadFace {
                source_face: u16::try_from(face_index)
                    .map_err(|_| "source face index exceeds QRP3")?,
                first_quad: face_first_quad,
                quad_count: face_quad_count,
                plane: surface.plane,
                material: surface.material,
                flags: if surface.face_flags & FACE_BACKSIDE != 0 {
                    RENDER_QUAD_FACE_BACKSIDE
                } else {
                    0
                },
            });
            input.runs.push(RenderQuadRun {
                first_quad: face_first_quad,
                quad_count: face_quad_count,
                material: surface.material,
                flags: RENDER_QUAD_RUN_PATCH_CLUT,
            });
        }
        let position_count = u16::try_from(local_positions.len())
            .map_err(|_| "QRP3 object position count exceeds u16")?;
        if position_count == 0 || position_count as usize > RENDER_QUAD_OBJECT_MAX_POSITIONS {
            return Err("QRP3 object position count exceeds its fixed-kernel limit".into());
        }
        input.positions.extend(local_positions);
        input.objects.push(RenderQuadObject {
            first_face,
            face_count: u16::try_from(object_faces.faces.len()).unwrap(),
            first_quad,
            quad_count: u16::try_from(object_faces.quads).unwrap(),
            first_position,
            position_count,
            first_run,
            run_count: u16::try_from(object_faces.faces.len()).unwrap(),
            mins,
            maxs,
            flags: 0,
        });
    }

    let mut fallback_bytes = Vec::with_capacity(view_faces.len());
    for (view_index, faces) in view_faces.iter().enumerate() {
        let bounds = source_bounds[view_index + 1];
        let mut bytes = 0usize;
        for &face_index in faces {
            let surface = &surfaces[face_index];
            if !surface.policy_visible {
                continue;
            }
            let plane = *planes
                .get(surface.plane as usize)
                .ok_or("fallback face plane is out of range")?;
            if leaf_invariant_facing(plane, surface.face_flags, bounds) == Some(false) {
                continue;
            }
            let root_triangles = surface.corners.len().saturating_sub(2);
            bytes += if surface.template_eligible {
                (root_triangles & 1) * 40
            } else {
                surface_packet_bytes(surface)
            };
        }
        fallback_bytes.push(bytes);
    }

    for view_index in 0..view_faces.len() {
        let bounds = source_bounds[view_index + 1];
        let mut commands = Vec::new();
        for (object_index, object) in objects.iter().enumerate() {
            let mut visible_faces = 0u32;
            let mut dynamic_faces = 0u32;
            for (local_face, &face_index) in object.faces.iter().enumerate() {
                if !signature_has(&signatures[face_index], view_index) {
                    continue;
                }
                let surface = &surfaces[face_index];
                let plane = *planes
                    .get(surface.plane as usize)
                    .ok_or("quad template face plane is out of range")?;
                let bit = 1u32 << local_face;
                match leaf_invariant_facing(plane, surface.face_flags, bounds) {
                    Some(false) => metrics.pruned_back_commands += 1,
                    Some(true) => {
                        visible_faces |= bit;
                        metrics.invariant_commands += 1;
                    }
                    None => {
                        visible_faces |= bit;
                        dynamic_faces |= bit;
                        metrics.dynamic_commands += 1;
                    }
                }
            }
            if visible_faces != 0 {
                commands.push(RenderQuadCommand {
                    object: u16::try_from(object_index)
                        .map_err(|_| "quad object count exceeds QRP3")?,
                    flags: 0,
                    visible_faces,
                    dynamic_faces,
                });
            }
        }
        let mut packet_pool_bytes = commands.iter().try_fold(0usize, |bytes, command| {
            let object = input
                .objects
                .get(command.object as usize)
                .ok_or("QRP3 cell command references a missing object")?;
            bytes
                .checked_add(object.quad_count as usize * RENDER_QUAD_PACKET_BYTES)
                .ok_or("QRP3 cell packet-pool size overflow")
        })?;
        while packet_pool_bytes + fallback_bytes[view_index] > RENDER_SECTION_GPU_TARGET_BYTES {
            let Some((spill_index, _, selected_bytes)) = commands
                .iter()
                .enumerate()
                .filter_map(|(command_index, command)| {
                    let object = &objects[command.object as usize];
                    let full_bytes = object.quads * RENDER_QUAD_PACKET_BYTES;
                    let selected_bytes = object
                        .faces
                        .iter()
                        .enumerate()
                        .filter(|(local_face, _)| command.visible_faces & (1 << local_face) != 0)
                        .map(|(_, face_index)| {
                            surfaces[*face_index].corners.len().saturating_sub(2) / 2
                                * RENDER_QUAD_PACKET_BYTES
                        })
                        .sum::<usize>();
                    (full_bytes > selected_bytes).then_some((
                        command_index,
                        full_bytes - selected_bytes,
                        selected_bytes,
                    ))
                })
                .max_by_key(|(_, saving, _)| *saving)
            else {
                break;
            };
            let command = commands.remove(spill_index);
            let object = &objects[command.object as usize];
            let full_bytes = object.quads * RENDER_QUAD_PACKET_BYTES;
            packet_pool_bytes -= full_bytes;
            fallback_bytes[view_index] += selected_bytes;
            metrics.spilled_commands += 1;
            metrics.spilled_faces += command.visible_faces.count_ones() as usize;
            metrics.spill_fallback_bytes += selected_bytes;
        }
        metrics.commands += commands.len();
        let (payload_bytes, activation_bytes, packet_pool_bytes) =
            leaf_local_quad_memory(&input, &commands)?;
        leaf_payload_bytes.push(payload_bytes);
        leaf_activation_bytes.push(activation_bytes);
        leaf_packet_pool_bytes.push(packet_pool_bytes);
        input.cells.push(RenderQuadCellInput {
            leaf: u16::try_from(view_index + 1).map_err(|_| "QRP3 leaf count exceeds u16")?,
            flags: 0,
            commands,
        });
    }

    let encoded = encode_render_quad_payload(&input)?;
    metrics.payload_bytes = encoded.bytes.len();
    metrics.activation_bytes =
        encoded.runtime_metadata_bytes as usize + encoded.projection_bytes as usize;
    metrics.objects = input.objects.len();
    metrics.quads = input.quads.len();
    metrics.positions = input.positions.len();
    metrics.fallback_p50_bytes = percentile(fallback_bytes.iter().copied(), 50);
    metrics.fallback_p95_bytes = percentile(fallback_bytes.iter().copied(), 95);
    metrics.fallback_max_bytes = fallback_bytes.iter().copied().max().unwrap_or(0);
    let leaf_total_bytes = leaf_packet_pool_bytes
        .iter()
        .copied()
        .zip(fallback_bytes.iter().copied())
        .map(|(packets, fallback)| packets + fallback)
        .collect::<Vec<_>>();
    metrics.leaf_payload_total_bytes = leaf_payload_bytes.iter().sum();
    metrics.leaf_payload_p50_bytes = percentile(leaf_payload_bytes.iter().copied(), 50);
    metrics.leaf_payload_p95_bytes = percentile(leaf_payload_bytes.iter().copied(), 95);
    metrics.leaf_payload_max_bytes = leaf_payload_bytes.into_iter().max().unwrap_or(0);
    metrics.leaf_activation_p50_bytes = percentile(leaf_activation_bytes.iter().copied(), 50);
    metrics.leaf_activation_p95_bytes = percentile(leaf_activation_bytes.iter().copied(), 95);
    metrics.leaf_activation_max_bytes = leaf_activation_bytes.into_iter().max().unwrap_or(0);
    metrics.leaf_total_p50_bytes = percentile(leaf_total_bytes.iter().copied(), 50);
    metrics.leaf_total_p95_bytes = percentile(leaf_total_bytes.iter().copied(), 95);
    metrics.leaf_total_max_bytes = leaf_total_bytes.into_iter().max().unwrap_or(0);

    let mut section_ranges = Vec::<(usize, usize, usize, usize, usize)>::new();
    let mut first_cell = 0usize;
    while first_cell < input.cells.len() {
        let mut end_cell = first_cell + 1;
        let (mut payload_bytes, mut activation_bytes, mut gpu_bytes) =
            section_quad_memory(&input, first_cell, end_cell, &fallback_bytes)?;
        while end_cell < input.cells.len() {
            let (candidate_payload, candidate_activation, candidate_gpu) =
                section_quad_memory(&input, first_cell, end_cell + 1, &fallback_bytes)?;
            if candidate_activation > RENDER_SECTION_CPU_ACTIVE_TARGET_BYTES
                || candidate_gpu > RENDER_SECTION_GPU_TARGET_BYTES
            {
                break;
            }
            end_cell += 1;
            payload_bytes = candidate_payload;
            activation_bytes = candidate_activation;
            gpu_bytes = candidate_gpu;
        }
        section_ranges.push((
            first_cell,
            end_cell,
            payload_bytes,
            activation_bytes,
            gpu_bytes,
        ));
        first_cell = end_cell;
    }
    let section_leaves = section_ranges
        .iter()
        .map(|(first, end, _, _, _)| end - first)
        .collect::<Vec<_>>();
    let section_activations = section_ranges
        .iter()
        .map(|(_, _, _, activation, _)| *activation)
        .collect::<Vec<_>>();
    let section_gpu = section_ranges
        .iter()
        .map(|(_, _, _, _, gpu)| *gpu)
        .collect::<Vec<_>>();
    metrics.section_count = section_ranges.len();
    metrics.section_oversize_count = section_activations
        .iter()
        .copied()
        .zip(section_gpu.iter().copied())
        .filter(|(activation, gpu)| {
            *activation > RENDER_SECTION_CPU_ACTIVE_TARGET_BYTES
                || *gpu > RENDER_SECTION_GPU_TARGET_BYTES
        })
        .count();
    metrics.section_payload_total_bytes = section_ranges
        .iter()
        .map(|(_, _, payload, _, _)| *payload)
        .sum();
    metrics.section_leaves_p50 = percentile(section_leaves.iter().copied(), 50);
    metrics.section_leaves_p95 = percentile(section_leaves.iter().copied(), 95);
    metrics.section_leaves_max = section_leaves.into_iter().max().unwrap_or(0);
    metrics.section_activation_p50_bytes = percentile(section_activations.iter().copied(), 50);
    metrics.section_activation_p95_bytes = percentile(section_activations.iter().copied(), 95);
    metrics.section_activation_max_bytes = section_activations.iter().copied().max().unwrap_or(0);
    metrics.section_gpu_p50_bytes = percentile(section_gpu.iter().copied(), 50);
    metrics.section_gpu_p95_bytes = percentile(section_gpu.iter().copied(), 95);
    metrics.section_gpu_max_bytes = section_gpu.iter().copied().max().unwrap_or(0);
    metrics.section_transition_max_bytes = metrics
        .section_activation_max_bytes
        .checked_add(
            section_ranges
                .iter()
                .map(|(_, _, payload, _, _)| *payload)
                .max()
                .unwrap_or(0),
        )
        .ok_or("QRS3 arbitrary-transition budget overflow")?;

    let mut leaf_sections = vec![RENDER_SECTION_NONE; input.cells.len() + 1];
    let mut section_inputs = Vec::with_capacity(section_ranges.len());
    let mut encoded_payload_total = 0usize;
    for (section_index, &(first, end, expected_payload, expected_activation, expected_gpu)) in
        section_ranges.iter().enumerate()
    {
        let payload = encode_local_quad_section(&input, first, end)?;
        if payload.bytes.len() != expected_payload {
            return Err(format!(
                "QRP3 section {section_index} payload accounting drifted: {} != {expected_payload}",
                payload.bytes.len()
            )
            .into());
        }
        let fallback = fallback_bytes[first..end]
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        let actual_activation =
            payload.runtime_metadata_bytes as usize + payload.projection_bytes as usize;
        if actual_activation != expected_activation {
            return Err(format!(
                "QRP3 section {section_index} CPU activation accounting drifted: {actual_activation} != {expected_activation}"
            )
            .into());
        }
        let actual_gpu = payload.packet_pool_bytes as usize + fallback;
        if actual_gpu != expected_gpu {
            return Err(format!(
                "QRP3 section {section_index} GPU accounting drifted: {actual_gpu} != {expected_gpu}"
            )
            .into());
        }
        for cell in &input.cells[first..end] {
            let leaf = cell.leaf as usize;
            if leaf >= leaf_sections.len() || leaf_sections[leaf] != RENDER_SECTION_NONE {
                return Err("QRS3 leaf assignment is invalid or duplicated".into());
            }
            leaf_sections[leaf] =
                u16::try_from(section_index).map_err(|_| "QRS3 section count exceeds u16")?;
        }
        let mut neighbors = Vec::with_capacity(2);
        if section_index != 0 {
            neighbors.push((section_index - 1) as u16);
        }
        if section_index + 1 != section_ranges.len() {
            neighbors.push((section_index + 1) as u16);
        }
        encoded_payload_total += payload.bytes.len();
        section_inputs.push(RenderSectionInput {
            neighbors,
            payload,
            fallback_bytes: u32::try_from(fallback)
                .map_err(|_| "QRS3 fallback workspace exceeds u32")?,
            flags: 0,
        });
    }
    if encoded_payload_total != metrics.section_payload_total_bytes {
        return Err("QRS3 encoded payload total drifted from the census".into());
    }
    let sidecar = encode_render_sections(
        &leaf_sections,
        &section_inputs,
        u32::try_from(resident_core_bytes).map_err(|_| "resident core exceeds QRS3 u32")?,
        u32::try_from(metrics.section_transition_max_bytes)
            .map_err(|_| "QRS3 streaming budget exceeds u32")?,
        u32::try_from(RENDER_SECTION_GPU_TARGET_BYTES)
            .map_err(|_| "QRS3 GPU budget exceeds u32")?,
    )?;
    metrics.section_sidecar_bytes = sidecar.len();
    Ok((metrics, sidecar))
}

fn load_census(path: &Path, map: &str, source: &Bsp<'_>) -> Result<MapCensus> {
    let bytes = fs::read(path)?;
    let mut index_reader = SliceReader::new(&bytes);
    let index = PsbIndex::read(&mut index_reader)
        .map_err(|error| format!("cannot read {} index: {error:?}", path.display()))?;
    let (resident_bytes, resident_core_bytes) = resident_and_collision_core_bytes(&index)?;
    let mut reader = SliceReader::new(&bytes);
    let mut resident = ResidentMap::with_capacity(RESIDENT_MAP_ARENA_BYTES);
    resident
        .load(1, &mut reader)
        .map_err(|error| format!("cannot load {}: {error:?}", path.display()))?;
    let indexed = resident
        .indexed_vertices()
        .ok_or_else(|| format!("{} is not an indexed PSB5 map", path.display()))?;
    let source_positions = indexed
        .positions
        .iter()
        .map(|position| position.position)
        .collect::<Vec<_>>();
    let faces = resident.faces();
    let textures = resident.textures();
    let surfaces = faces
        .iter()
        .map(|face| {
            let first = face.first_vertex as usize;
            let end = first + face.vertex_count as usize;
            let texture = textures
                .get(face.texture as usize)
                .expect("validated face texture index");
            let baked =
                face.flags & (FACE_BAKED_UV | FACE_BAKED_LIGHT) == FACE_BAKED_UV | FACE_BAKED_LIGHT;
            let animated = texture.animation_total > 0;
            let special = texture.flags
                & (TEXTURE_LIQUID
                    | TEXTURE_SKY
                    | TEXTURE_LAYERED_SKY
                    | TEXTURE_INVISIBLE
                    | TEXTURE_NULL)
                != 0;
            let corners = indexed.corners[first..end]
                .iter()
                .map(|corner| SurfaceCorner {
                    position: corner.position_index,
                    uv: u16::from(corner.texture[0]) | (u16::from(corner.texture[1]) << 8),
                    color: corner.light,
                })
                .collect::<Vec<_>>();
            let positions = corners
                .iter()
                .map(|corner| corner.position)
                .collect::<Vec<_>>();
            let mut mins = [i16::MAX; 3];
            let mut maxs = [i16::MIN; 3];
            for &position_index in &positions {
                let position = indexed.positions[position_index as usize].position;
                for axis in 0..3 {
                    mins[axis] = mins[axis].min(position[axis]);
                    maxs[axis] = maxs[axis].max(position[axis]);
                }
            }
            Surface {
                plane: face.plane as u16,
                face_flags: face.flags,
                material: face.texture as u16,
                template_eligible: baked && !special && !animated,
                policy_visible: texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) == 0,
                liquid: texture.flags & TEXTURE_LIQUID != 0,
                mins,
                maxs,
                positions,
                corners,
                tpage: texture.texture_page,
            }
        })
        .collect::<Vec<_>>();

    let leaves = resident.leaves();
    let marks = resident.mark_surfaces();
    let visible_leaves = resident
        .brush_models()
        .get(0)
        .ok_or_else(|| format!("{map} has no world brush"))?
        .visible_leaves
        .max(0) as usize;
    let signature_words = (leaves.len().saturating_sub(1) + 63) >> 6;
    let mut signatures = vec![vec![0u64; signature_words]; surfaces.len()];
    let mut row = vec![0u8; (visible_leaves + 7) >> 3];
    let mut face_marked = vec![false; surfaces.len()];
    let mut view_faces = vec![Vec::<usize>::new(); leaves.len().saturating_sub(1)];

    for camera_leaf in 1..leaves.len() {
        row.fill(0);
        let Some(addressable_leaves) = resident.leaf_visibility_into(camera_leaf, &mut row) else {
            continue;
        };
        face_marked.fill(false);
        for visible_index in 0..addressable_leaves {
            if row[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                continue;
            }
            let leaf = leaves
                .get(visible_index + 1)
                .ok_or_else(|| format!("{map} PVS references missing leaf"))?;
            let start = leaf.first_mark_surface as usize;
            let end = start + leaf.mark_surface_count as usize;
            for mark_index in start..end {
                let face_index = marks
                    .get(mark_index)
                    .ok_or_else(|| format!("{map} leaf mark range is truncated"))?
                    as usize;
                *face_marked
                    .get_mut(face_index)
                    .ok_or_else(|| format!("{map} mark references face {face_index}"))? = true;
            }
        }
        let bit = camera_leaf - 1;
        let faces_for_view = &mut view_faces[bit];
        for (face_index, marked) in face_marked.iter().copied().enumerate() {
            if !marked {
                continue;
            }
            signatures[face_index][bit >> 6] |= 1u64 << (bit & 63);
            faces_for_view.push(face_index);
        }
    }

    let ordered_batches = ordered_batches(&surfaces, &signatures);
    let connected_batches = connected_batches(&surfaces, &signatures);
    let surface_packet_bytes = surfaces
        .iter()
        .map(surface_packet_bytes)
        .collect::<Vec<_>>();
    let source_bounds = source_leaf_bounds(source);
    if source_bounds.len() != leaves.len() {
        return Err(format!(
            "{map} source/cooked leaf count differs: {} != {}",
            source_bounds.len(),
            leaves.len(),
        )
        .into());
    }
    let planes = resident.planes().iter().collect::<Vec<_>>();
    let mut facing_pairs = 0usize;
    let mut invariant_facing_pairs = 0usize;
    let mut views = view_faces
        .iter()
        .enumerate()
        .map(|(view_index, faces)| view_metrics(view_index, faces, &surfaces, &connected_batches))
        .collect::<Vec<_>>();
    for (view_index, faces) in view_faces.iter().enumerate() {
        let bounds = source_bounds[view_index + 1];
        let (_, cell) = cell_stream_metrics(faces, &surfaces, &planes, bounds)?;
        views[view_index].cell_faces = cell.cell_faces;
        views[view_index].cell_dynamic_facing = cell.cell_dynamic_facing;
        views[view_index].cell_invariant_front = cell.cell_invariant_front;
        views[view_index].cell_invariant_back_pruned = cell.cell_invariant_back_pruned;
        views[view_index].cell_policy_pruned = cell.cell_policy_pruned;
        views[view_index].cell_blocks = cell.cell_blocks;
        views[view_index].cell_stream_bytes = cell.cell_stream_bytes;
        let mut ambiguous = 0usize;
        for &face_index in faces {
            facing_pairs += 1;
            let surface = &surfaces[face_index];
            let plane = planes
                .get(surface.plane as usize)
                .ok_or_else(|| format!("{map} face plane is out of range"))?;
            if facing_is_leaf_invariant(*plane, bounds) {
                invariant_facing_pairs += 1;
            } else {
                ambiguous += 1;
            }
            if surface.template_eligible {
                let packet_bytes = surface_packet_bytes[face_index];
                views[view_index].eligible_packet_bytes += packet_bytes;
                match leaf_invariant_facing(*plane, surface.face_flags, bounds) {
                    Some(true) => {
                        views[view_index].invariant_front_packet_bytes += packet_bytes;
                    }
                    Some(false) => {
                        views[view_index].invariant_back_packet_bytes += packet_bytes;
                    }
                    None => {
                        views[view_index].ambiguous_template_packet_bytes += packet_bytes;
                    }
                }
            }
        }
        views[view_index].ambiguous_facing = ambiguous;
    }
    let (quad_payload, render_sections) = build_quad_payload(
        &surfaces,
        &signatures,
        &view_faces,
        &planes,
        &source_bounds,
        &source_positions,
        resident_core_bytes,
    )?;
    let masked_objects =
        masked_object_metrics(&surfaces, &signatures, &view_faces, &planes, &source_bounds)?;
    let pvs_faces = views.iter().map(|view| view.faces).max().unwrap_or(0);
    let visibility_classes = signatures
        .iter()
        .filter(|signature| !signature_is_empty(signature))
        .collect::<BTreeSet<_>>()
        .len();

    Ok(MapCensus {
        map: map.to_owned(),
        faces: signatures
            .iter()
            .filter(|signature| !signature_is_empty(signature))
            .count(),
        pvs_faces,
        positions: indexed.positions.len(),
        visibility_classes,
        ordered_batches,
        connected_batches,
        surface_packet_bytes,
        views,
        facing_pairs,
        invariant_facing_pairs,
        quad_payload,
        masked_objects,
        resident_bytes,
        resident_core_bytes,
        render_sections,
    })
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn print_map(census: &MapCensus) {
    let nonempty_views = census.views.iter().filter(|view| view.faces != 0);
    let view_count = nonempty_views.clone().count();
    let total_faces: usize = nonempty_views.clone().map(|view| view.faces).sum();
    let total_batches: usize = nonempty_views.clone().map(|view| view.active_batches).sum();
    let total_position_refs: usize = nonempty_views
        .clone()
        .map(|view| view.active_position_references)
        .sum();
    let total_unique_positions: usize = nonempty_views
        .clone()
        .map(|view| view.unique_positions)
        .sum();
    let total_corner_references: usize = nonempty_views
        .clone()
        .map(|view| view.corner_references)
        .sum();
    let total_reordered: usize = nonempty_views
        .clone()
        .map(|view| view.reordered_faces)
        .sum();
    let material_references: usize = nonempty_views
        .clone()
        .map(|view| view.material_references)
        .sum();
    let material_changes: usize = nonempty_views
        .clone()
        .map(|view| view.material_changes)
        .sum();
    let contiguous = census
        .connected_batches
        .iter()
        .filter(|batch| batch.is_contiguous())
        .count();
    let connected_faces: usize = census
        .connected_batches
        .iter()
        .map(|batch| batch.faces.len())
        .sum();
    let connected_positions: usize = census
        .connected_batches
        .iter()
        .map(|batch| batch.positions.len())
        .sum();
    let worst_batches = census
        .views
        .iter()
        .map(|view| view.active_batches)
        .max()
        .unwrap_or(0);
    let worst_position_refs = census
        .views
        .iter()
        .map(|view| view.active_position_references)
        .max()
        .unwrap_or(0);
    let worst_unique_positions = census
        .views
        .iter()
        .max_by_key(|view| view.active_position_references)
        .map_or(0, |view| view.unique_positions);

    println!(
        "| {} | {} | {} | {} | {} | {} | {:.2} | {} | {:.2} | {:.1}% | {:.1}% | {:.1}% |",
        census.map,
        census.faces,
        census.pvs_faces,
        census.positions,
        census.visibility_classes,
        census.connected_batches.len(),
        ratio(connected_faces, census.connected_batches.len()),
        worst_batches,
        ratio(total_faces, total_batches),
        ratio(
            total_position_refs.saturating_sub(total_unique_positions),
            total_unique_positions,
        ) * 100.0,
        ratio(total_reordered, total_faces) * 100.0,
        ratio(census.invariant_facing_pairs, census.facing_pairs) * 100.0,
    );
    println!(
        "  {}: ordered-ranges={} ({:.2} faces/range), connected-range positions={} ({:.2}/range), contiguous={}/{}; base packet templates p50/p95/max={}/{}/{} KiB; leaf-local invariant-front p50/p95/max={}/{}/{} KiB, ambiguous-template={}/{}/{} KiB; active positions p50/p95/max={}/{}/{}, worst duplication={}/{}, unique/corners={:.1}%; ambiguous facing p50/p95/max={}/{}/{}; material retention={:.1}%; views={}",
        census.map,
        census.ordered_batches.len(),
        ratio(census.faces, census.ordered_batches.len()),
        connected_positions,
        ratio(connected_positions, census.connected_batches.len()),
        contiguous,
        census.connected_batches.len(),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.base_packet_bytes),
            50,
        ) / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.base_packet_bytes),
            95,
        ) / 1024,
        census
            .views
            .iter()
            .map(|view| view.base_packet_bytes)
            .max()
            .unwrap_or(0)
            / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.invariant_front_packet_bytes),
            50,
        ) / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.invariant_front_packet_bytes),
            95,
        ) / 1024,
        census
            .views
            .iter()
            .map(|view| view.invariant_front_packet_bytes)
            .max()
            .unwrap_or(0)
            / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.ambiguous_template_packet_bytes),
            50,
        ) / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.ambiguous_template_packet_bytes),
            95,
        ) / 1024,
        census
            .views
            .iter()
            .map(|view| view.ambiguous_template_packet_bytes)
            .max()
            .unwrap_or(0)
            / 1024,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.active_position_references),
            50,
        ),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.active_position_references),
            95,
        ),
        worst_position_refs,
        worst_position_refs,
        worst_unique_positions,
        ratio(total_unique_positions, total_corner_references) * 100.0,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.ambiguous_facing),
            50,
        ),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.ambiguous_facing),
            95,
        ),
        census
            .views
            .iter()
            .map(|view| view.ambiguous_facing)
            .max()
            .unwrap_or(0),
        (1.0 - ratio(material_changes, material_references)) * 100.0,
        view_count,
    );
    let qrp = census.quad_payload;
    println!(
        "  {}: real QRP3 base subset={} faces/{} quads/{} objects/{} positions, compact payload={} KiB, activated CPU metadata+projection={} KiB; cell commands={} ({} invariant, {} dynamic, {} invariant-back pruned), GPU-cap spills={} commands/{} faces/{} KiB; excluded faces={}, baked overflow corners={}, odd GT3 source fallbacks={}; base fallback packet bytes p50/p95/max={}/{}/{} KiB",
        census.map,
        qrp.template_faces,
        qrp.quads,
        qrp.objects,
        qrp.positions,
        qrp.payload_bytes / 1024,
        qrp.activation_bytes / 1024,
        qrp.commands,
        qrp.invariant_commands,
        qrp.dynamic_commands,
        qrp.pruned_back_commands,
        qrp.spilled_commands,
        qrp.spilled_faces,
        qrp.spill_fallback_bytes / 1024,
        qrp.excluded_faces,
        qrp.baked_overflow_corners,
        qrp.odd_fallback_triangles,
        qrp.fallback_p50_bytes / 1024,
        qrp.fallback_p95_bytes / 1024,
        qrp.fallback_max_bytes / 1024,
    );
    println!(
        "  {}: leaf-local QRP3 payload p50/p95/max={}/{}/{} KiB ({} KiB duplicated on disc); CPU activation p50/p95/max={}/{}/{} KiB; active GPU base+fallback p50/p95/max={}/{}/{} KiB",
        census.map,
        qrp.leaf_payload_p50_bytes / 1024,
        qrp.leaf_payload_p95_bytes / 1024,
        qrp.leaf_payload_max_bytes / 1024,
        qrp.leaf_payload_total_bytes / 1024,
        qrp.leaf_activation_p50_bytes / 1024,
        qrp.leaf_activation_p95_bytes / 1024,
        qrp.leaf_activation_max_bytes / 1024,
        qrp.leaf_total_p50_bytes / 1024,
        qrp.leaf_total_p95_bytes / 1024,
        qrp.leaf_total_max_bytes / 1024,
    );
    println!(
        "  {}: checked consecutive-leaf QRS3 CPU/GPU targets {}/{} KiB -> {} sections ({} oversize), leaves p50/p95/max={}/{}/{}, CPU activation p50/p95/max={}/{}/{} KiB, GPU high-water p50/p95/max={}/{}/{} KiB, payload/complete sidecar={}/{} KiB, worst CPU active+arbitrary-payload preload={} KiB",
        census.map,
        RENDER_SECTION_CPU_ACTIVE_TARGET_BYTES / 1024,
        RENDER_SECTION_GPU_TARGET_BYTES / 1024,
        qrp.section_count,
        qrp.section_oversize_count,
        qrp.section_leaves_p50,
        qrp.section_leaves_p95,
        qrp.section_leaves_max,
        qrp.section_activation_p50_bytes / 1024,
        qrp.section_activation_p95_bytes / 1024,
        qrp.section_activation_max_bytes / 1024,
        qrp.section_gpu_p50_bytes / 1024,
        qrp.section_gpu_p95_bytes / 1024,
        qrp.section_gpu_max_bytes / 1024,
        qrp.section_payload_total_bytes / 1024,
        qrp.section_sidecar_bytes / 1024,
        qrp.section_transition_max_bytes / 1024,
    );
    let split_high_water = census.resident_core_bytes + qrp.section_transition_max_bytes;
    println!(
        "  {}: resident split current/core/reclaimed={}/{}/{} KiB; core + worst QRS3 transition={} KiB, arena headroom={} KiB",
        census.map,
        census.resident_bytes / 1024,
        census.resident_core_bytes / 1024,
        census
            .resident_bytes
            .saturating_sub(census.resident_core_bytes)
            / 1024,
        split_high_water / 1024,
        (RESIDENT_MAP_ARENA_BYTES as isize - split_high_water as isize) / 1024,
    );
    let masked = census.masked_objects;
    println!(
        "  {}: source-order masked objects={} for {} faces ({:.2} faces/object); commands mean/p50/p95/max={:.2}/{}/{}/{}, selected/projected quads mean={:.2}/{:.2}, quad overprojection={:.1}%, position overprojection={:.1}%",
        census.map,
        masked.objects,
        masked.faces,
        ratio(masked.faces, masked.objects),
        ratio(masked.commands, view_count),
        masked.commands_p50,
        masked.commands_p95,
        masked.commands_max,
        ratio(masked.selected_quads, view_count),
        ratio(masked.projected_quads, view_count),
        ratio(
            masked.projected_quads.saturating_sub(masked.selected_quads),
            masked.selected_quads,
        ) * 100.0,
        ratio(
            masked
                .projected_positions
                .saturating_sub(masked.selected_positions),
            masked.selected_positions,
        ) * 100.0,
    );
    let cell_faces: usize = nonempty_views.clone().map(|view| view.cell_faces).sum();
    let cell_dynamic: usize = nonempty_views
        .clone()
        .map(|view| view.cell_dynamic_facing)
        .sum();
    let cell_front: usize = nonempty_views
        .clone()
        .map(|view| view.cell_invariant_front)
        .sum();
    let cell_back: usize = nonempty_views
        .clone()
        .map(|view| view.cell_invariant_back_pruned)
        .sum();
    let cell_policy: usize = nonempty_views
        .clone()
        .map(|view| view.cell_policy_pruned)
        .sum();
    let cell_stream_bytes: usize = nonempty_views
        .clone()
        .map(|view| view.cell_stream_bytes)
        .sum();
    println!(
        "  {}: retained cell stream candidates={} ({:.1}% of PVS), dynamic plane tests={} ({:.1}% of PVS), invariant fronts={}, pruned invariant backs={}, pruned null/invisible={}; stream p50/p95/max={}/{}/{} bytes, blocks p50/p95/max={}/{}/{}, all-leaf bake={} KiB",
        census.map,
        cell_faces,
        ratio(cell_faces, total_faces) * 100.0,
        cell_dynamic,
        ratio(cell_dynamic, total_faces) * 100.0,
        cell_front,
        cell_back,
        cell_policy,
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.cell_stream_bytes),
            50,
        ),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.cell_stream_bytes),
            95,
        ),
        census
            .views
            .iter()
            .map(|view| view.cell_stream_bytes)
            .max()
            .unwrap_or(0),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.cell_blocks),
            50,
        ),
        percentile(
            census
                .views
                .iter()
                .filter(|view| view.faces != 0)
                .map(|view| view.cell_blocks),
            95,
        ),
        census
            .views
            .iter()
            .map(|view| view.cell_blocks)
            .max()
            .unwrap_or(0),
        cell_stream_bytes / 1024,
    );
    for prefix_kib in HOT_PREFIX_KIB {
        let hot = hot_prefix_metrics(
            &census.connected_batches,
            &census.surface_packet_bytes,
            &census.views,
            prefix_kib * 1024,
        );
        println!(
            "  {}: hot prefix {} KiB -> {} KiB / {} batches resident, packet-visit coverage {:.1}%, static p95/max={}/{} KiB, other-packet headroom={} KiB, base-only overflow={}/{} views",
            census.map,
            prefix_kib,
            hot.resident_bytes / 1024,
            hot.resident_batches,
            ratio(hot.resident_visit_bytes, hot.total_visit_bytes) * 100.0,
            hot.p95_static_high_water / 1024,
            hot.maximum_static_high_water / 1024,
            hot.minimum_other_headroom / 1024,
            hot.overflowing_views,
            hot.nonempty_views,
        );
    }
}

fn main() -> Result<()> {
    let mut arguments = env::args_os().skip(1);
    let maps_dir = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("id1psx/maps"));
    let pak_path = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".quakepsx/cache/shareware/ID1/PAK0.PAK"));
    let render_section_dir = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        return Err(
            "usage: quake2-transfer-census [cooked-maps-dir] [pak0.pak] [qrs-output-dir]"
                .into(),
        );
    }
    if let Some(directory) = &render_section_dir {
        fs::create_dir_all(directory)?;
    }
    let pak_bytes = fs::read(&pak_path)?;
    let pak = PakArchive::parse(&pak_bytes)?;
    println!("# Quake II PSX static-brush transfer census");
    println!();
    println!(
        "Exact camera-leaf visibility signatures; connected ranges share a complete boundary edge and obey the retail {}-face/{}-position caps.",
        RETAIL_FACE_LIMIT, RETAIL_VERTEX_LIMIT
    );
    println!();
    println!("| map | world faces | max PVS | positions | vis classes | connected ranges | faces/range | max active ranges | active faces/range | position duplication | reorder pressure | leaf-invariant facing |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    let mut censuses = Vec::new();
    for map in MAPS {
        let source = Bsp::parse(pak.require(&format!("maps/{map}.bsp"))?)?;
        let census = load_census(&maps_dir.join(format!("{map}.psb")), map, &source)?;
        if let Some(directory) = &render_section_dir {
            let destination = directory.join(format!("{map}.qrs"));
            let temporary = directory.join(format!(".{map}.qrs.tmp"));
            fs::write(&temporary, &census.render_sections)?;
            fs::rename(&temporary, &destination)?;
        }
        print_map(&census);
        censuses.push(census);
    }
    if let Some(directory) = &render_section_dir {
        println!();
        println!("Wrote checked QRS3 sidecars to {}", directory.display());
    }

    let faces: usize = censuses.iter().map(|census| census.faces).sum();
    let ordered: usize = censuses
        .iter()
        .map(|census| census.ordered_batches.len())
        .sum();
    let connected: usize = censuses
        .iter()
        .map(|census| census.connected_batches.len())
        .sum();
    let classes: usize = censuses
        .iter()
        .map(|census| census.visibility_classes)
        .sum();
    let facing_pairs: usize = censuses.iter().map(|census| census.facing_pairs).sum();
    let invariant_facing_pairs: usize = censuses
        .iter()
        .map(|census| census.invariant_facing_pairs)
        .sum();
    println!();
    println!(
        "Episode 1 totals: {faces} PVS-addressable world faces, {classes} exact visibility classes, {ordered} source-order ranges ({:.2} faces/range), {connected} shared-edge ranges ({:.2} faces/range), {:.2}% of leaf/face facing decisions invariant from conservative source-leaf AABBs.",
        ratio(faces, ordered),
        ratio(faces, connected),
        ratio(invariant_facing_pairs, facing_pairs) * 100.0,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(material: u16, positions: &[u16]) -> Surface {
        Surface {
            plane: 0,
            face_flags: 0,
            material,
            template_eligible: true,
            policy_visible: true,
            liquid: false,
            mins: [0; 3],
            maxs: [1; 3],
            positions: positions.to_vec(),
            corners: positions
                .iter()
                .copied()
                .map(|position| SurfaceCorner {
                    position,
                    uv: position,
                    color: u32::from(position),
                })
                .collect(),
            tpage: material,
        }
    }

    #[test]
    fn cell_stream_roundtrips_delta_escape_and_facing_mode() {
        let mut surfaces = vec![surface(0, &[0, 1, 2]); 300];
        for (index, surface) in surfaces.iter_mut().enumerate() {
            surface.mins = [index as i16, -2, -3];
            surface.maxs = [index as i16 + 1, 4, 5];
        }
        let entries = vec![
            CellFace {
                face: 3,
                dynamic_facing: false,
            },
            CellFace {
                face: 4,
                dynamic_facing: true,
            },
            CellFace {
                face: 200,
                dynamic_facing: false,
            },
        ];
        let encoded = encode_cell_stream(&entries, &surfaces);
        assert_eq!(
            decode_cell_stream(&encoded).as_deref(),
            Some(entries.as_slice())
        );
        assert_eq!(encoded.len(), CELL_STREAM_BLOCK_HEADER_BYTES + 5);
    }

    #[test]
    fn leaf_aabb_proves_only_planes_that_do_not_cross_it() {
        let axial = Plane {
            normal: quake_formats::Vec3I16 {
                x: 4096,
                y: 0,
                z: 0,
            },
            distance: 20 * 4096,
            kind: 0,
        };
        let bounds = LeafBounds {
            mins: [0, -4, -4],
            maxs: [10, 4, 4],
        };
        assert!(facing_is_leaf_invariant(axial, bounds));
        assert!(!facing_is_leaf_invariant(
            Plane {
                distance: 5 * 4096,
                ..axial
            },
            bounds,
        ));
    }

    #[test]
    fn exact_visibility_prevents_cross_class_batching() {
        let surfaces = vec![
            surface(0, &[0, 1, 2, 3]),
            surface(0, &[1, 4, 5, 2]),
            surface(0, &[4, 6, 7, 5]),
        ];
        let signatures = vec![vec![1], vec![1], vec![2]];
        let batches = connected_batches(&surfaces, &signatures);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].faces, [0, 1]);
        assert_eq!(batches[0].positions, [0, 1, 2, 3, 4, 5]);
        assert_eq!(batches[1].faces, [2]);
    }

    #[test]
    fn a_single_shared_corner_does_not_merge_brushes() {
        let surfaces = vec![surface(0, &[0, 1, 2]), surface(0, &[2, 3, 4])];
        let batches = connected_batches(&surfaces, &[vec![1], vec![1]]);
        assert_eq!(batches.len(), 2);
    }

    #[test]
    fn retail_face_limit_splits_large_components() {
        let mut surfaces = Vec::new();
        for index in 0..33u16 {
            surfaces.push(surface(0, &[index, index + 1, index + 2]));
        }
        let signatures = vec![vec![1]; surfaces.len()];
        let batches = connected_batches(&surfaces, &signatures);
        assert_eq!(
            batches.iter().map(|batch| batch.faces.len()).sum::<usize>(),
            33
        );
        assert_eq!(batches[0].faces.len(), RETAIL_FACE_LIMIT);
        assert_eq!(batches[1].faces.len(), 1);
    }

    #[test]
    fn view_metrics_exposes_order_changes() {
        let surfaces = vec![
            surface(0, &[0, 1, 2]),
            surface(1, &[3, 4, 5]),
            surface(0, &[1, 6, 2]),
        ];
        let batches = vec![
            Batch {
                signature: vec![1],
                faces: vec![0, 2],
                positions: vec![0, 1, 2, 6],
            },
            Batch {
                signature: vec![1],
                faces: vec![1],
                positions: vec![3, 4, 5],
            },
        ];
        let metrics = view_metrics(0, &[0, 1, 2], &surfaces, &batches);
        assert_eq!(metrics.faces, 3);
        assert_eq!(metrics.active_batches, 2);
        assert_eq!(metrics.reordered_faces, 2);
        assert_eq!(metrics.material_changes, 2);
    }

    #[test]
    fn hot_prefix_counts_only_resident_batches_active_in_each_view() {
        let batches = vec![
            Batch {
                signature: vec![0b11],
                faces: vec![0],
                positions: vec![0, 1, 2, 3],
            },
            Batch {
                signature: vec![0b01],
                faces: vec![1],
                positions: vec![4, 5, 6],
            },
            Batch {
                signature: vec![0b10],
                faces: vec![2],
                positions: vec![7, 8, 9, 10, 11],
            },
        ];
        let views = vec![
            ViewMetrics {
                faces: 2,
                base_packet_bytes: 92,
                ..ViewMetrics::default()
            },
            ViewMetrics {
                faces: 2,
                base_packet_bytes: 144,
                ..ViewMetrics::default()
            },
        ];
        let metrics = hot_prefix_metrics(&batches, &[52, 40, 92], &views, 52);
        assert_eq!(metrics.resident_bytes, 52);
        assert_eq!(metrics.resident_batches, 1);
        assert_eq!(metrics.resident_visit_bytes, 104);
        assert_eq!(metrics.total_visit_bytes, 236);
        assert_eq!(metrics.p95_static_high_water, 92);
        assert_eq!(metrics.maximum_static_high_water, 144);
        assert_eq!(metrics.overflowing_views, 0);
        assert_eq!(metrics.nonempty_views, 2);
    }
}
