//! Golden behavior captured from Quake commit `4eed5867`.
//!
//! Every case runs through the production Quake-space psx-bsp facade and a
//! direct shared provider. Both use caller-owned `TraceScratch` and the exact
//! Quake Z-up to PSoXide Y-up boundary, while the fixed expected results retain
//! the original implementation's behavior.

use psx_bsp::collision::{
    CollisionHull as SharedCollisionHull, Trace as SharedTrace, TraceFlag,
    TraceScratch as SharedTraceScratch, TransformedCollisionHull as SharedTransformedCollisionHull,
    TRACE_STACK_CAPACITY,
};
use psx_bsp::{ClipNode as SharedClipNode, Plane as SharedPlane, RecordSlice as SharedRecordSlice};
use quake_core::bsp_axis_adapter::{
    psoxide_normal_to_quake, psoxide_point_to_quake, psoxide_rotation_to_quake,
    quake_normal_to_psoxide, quake_point_to_psoxide, quake_raw_hull_transform_to_psoxide,
    quake_rotation_to_psoxide, RotationQ12, IDENTITY_ROTATION_Q12,
};
use quake_core::collision::{
    BrushTransform as QuakeBrushTransform, CollisionHull, Trace, CONTENTS_EMPTY, CONTENTS_SOLID,
    CONTENTS_WATER, Q12_ONE,
};
use quake_formats::{ClipNode, Plane, RecordSlice, Vec3I16, Vec3I32};

const CONTENTS_SLIME: i16 = -4;
const CONTENTS_LAVA: i16 = -5;
const CONTENTS_SKY: i16 = -6;
const PLANE_EPSILON_Q12: i32 = 128;

#[derive(Copy, Clone)]
struct PlaneSpec {
    normal: Vec3I16,
    distance: i32,
    kind: i32,
}

#[derive(Copy, Clone)]
struct NodeSpec {
    plane: i16,
    children: [i16; 2],
}

struct OracleHull {
    planes: Vec<u8>,
    nodes: Vec<u8>,
    head_node: i16,
}

impl OracleHull {
    fn new(planes: &[PlaneSpec], nodes: &[NodeSpec], head_node: i16) -> Self {
        let mut plane_bytes = Vec::with_capacity(planes.len() * 14);
        for plane in planes {
            plane_bytes.extend_from_slice(&plane.normal.x.to_le_bytes());
            plane_bytes.extend_from_slice(&plane.normal.y.to_le_bytes());
            plane_bytes.extend_from_slice(&plane.normal.z.to_le_bytes());
            plane_bytes.extend_from_slice(&plane.distance.to_le_bytes());
            plane_bytes.extend_from_slice(&plane.kind.to_le_bytes());
        }

        let mut node_bytes = Vec::with_capacity(nodes.len() * 6);
        for node in nodes {
            node_bytes.extend_from_slice(&node.plane.to_le_bytes());
            node_bytes.extend_from_slice(&node.children[0].to_le_bytes());
            node_bytes.extend_from_slice(&node.children[1].to_le_bytes());
        }

        Self {
            planes: plane_bytes,
            nodes: node_bytes,
            head_node,
        }
    }

    fn collision_hull(&self) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<Plane>::new(&self.planes).unwrap(),
            RecordSlice::<ClipNode>::new(&self.nodes).unwrap(),
            self.head_node,
        )
        .unwrap()
    }

    fn shared_local_hull(&self) -> SharedCollisionHull<'_> {
        SharedCollisionHull::new(
            SharedRecordSlice::<SharedPlane>::new(&self.planes).unwrap(),
            SharedRecordSlice::<SharedClipNode>::new(&self.nodes).unwrap(),
            self.head_node,
        )
        .unwrap()
    }

    fn shared_axis_hull(&self) -> SharedTransformedCollisionHull<'_> {
        self.shared_local_hull()
            .transformed(quake_raw_hull_transform_to_psoxide(
                Vec3I32::default(),
                IDENTITY_ROTATION_Q12,
            ))
    }

    fn shared_trace_with_scratch(
        &self,
        start: Vec3I32,
        end: Vec3I32,
        scratch: &mut SharedTraceScratch,
        output: &mut SharedTrace,
    ) -> bool {
        self.shared_axis_hull().trace_into(
            &quake_point_to_psoxide(start),
            &quake_point_to_psoxide(end),
            scratch,
            output,
        )
    }

    fn shared_trace(&self, start: Vec3I32, end: Vec3I32) -> Option<Trace> {
        let mut output = SharedTrace::default();
        self.shared_trace_with_scratch(start, end, &mut SharedTraceScratch::new(), &mut output)
            .then(|| shared_trace_to_quake(output))
    }

    fn shared_point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.shared_axis_hull()
            .point_contents(quake_point_to_psoxide(point))
    }
}

fn shared_trace_to_quake(trace: SharedTrace) -> Trace {
    Trace {
        all_solid: trace.all_solid.is_set(),
        start_solid: trace.start_solid.is_set(),
        in_open: trace.in_open.is_set(),
        in_water: trace.in_water.is_set(),
        fraction: trace.fraction,
        end: psoxide_point_to_quake(trace.end),
        normal: psoxide_normal_to_quake(trace.normal),
        plane_distance: trace.plane_distance,
    }
}

/// Compatibility seam for running these cases against the future adapter.
trait TraceProvider {
    fn trace(&self, start: Vec3I32, end: Vec3I32) -> Option<Trace>;
    fn point_contents(&self, point: Vec3I32) -> Option<i16>;
}

impl TraceProvider for OracleHull {
    fn trace(&self, start: Vec3I32, end: Vec3I32) -> Option<Trace> {
        let mut output = Trace::default();
        self.collision_hull()
            .trace_into(
                &start,
                &end,
                &mut quake_core::collision::TraceScratch::new(),
                &mut output,
            )
            .then_some(output)
    }

    fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.collision_hull().point_contents(point)
    }
}

#[derive(Copy, Clone)]
struct GoldenTraceCase {
    name: &'static str,
    start: Vec3I32,
    end: Vec3I32,
    expected: Trace,
}

fn assert_golden(provider: &OracleHull, case: GoldenTraceCase) {
    let expected = Some(case.expected);
    assert_eq!(
        provider.trace(case.start, case.end),
        expected,
        "local golden trace case {:?}",
        case.name
    );
    assert_eq!(
        provider.shared_trace(case.start, case.end),
        expected,
        "shared golden trace case {:?}",
        case.name
    );
}

fn plane(normal: Vec3I16, kind: i32) -> PlaneSpec {
    PlaneSpec {
        normal,
        distance: 0,
        kind,
    }
}

fn one_plane_hull(normal: Vec3I16, kind: i32) -> OracleHull {
    OracleHull::new(
        &[plane(normal, kind)],
        &[NodeSpec {
            plane: 0,
            children: [CONTENTS_EMPTY, CONTENTS_SOLID],
        }],
        0,
    )
}

fn deep_crossing_hull(depth: usize) -> OracleHull {
    let mut planes = Vec::with_capacity(depth);
    let mut nodes = Vec::with_capacity(depth);
    for index in 0..depth {
        planes.push(PlaneSpec {
            normal: Vec3I16 {
                x: Q12_ONE as i16,
                y: 0,
                z: 0,
            },
            distance: index as i32 * Q12_ONE,
            kind: 0,
        });
        nodes.push(NodeSpec {
            plane: index as i16,
            children: [
                if index + 1 == depth {
                    CONTENTS_EMPTY
                } else {
                    (index + 1) as i16
                },
                CONTENTS_SOLID,
            ],
        });
    }
    OracleHull::new(&planes, &nodes, 0)
}

fn shared_transformed_trace(
    hull: &OracleHull,
    origin: Vec3I32,
    rotation: RotationQ12,
    start: Vec3I32,
    end: Vec3I32,
    scratch: &mut SharedTraceScratch,
) -> Option<Trace> {
    let transformed = hull
        .collision_hull()
        .transformed(QuakeBrushTransform { origin, rotation });
    let mut output = Trace::default();
    transformed
        .trace_into(&start, &end, scratch, &mut output)
        .then_some(output)
}

fn unobstructed(end: Vec3I32) -> Trace {
    Trace {
        all_solid: false,
        start_solid: false,
        in_open: true,
        in_water: false,
        fraction: Q12_ONE,
        end,
        normal: Vec3I16::default(),
        plane_distance: 0,
    }
}

#[test]
fn oracle_pins_unobstructed_and_zero_length_traces() {
    let empty = OracleHull::new(&[], &[], CONTENTS_EMPTY);
    let destination = Vec3I32 {
        x: 7 << 12,
        y: -3 << 12,
        z: 11 << 12,
    };
    assert_golden(
        &empty,
        GoldenTraceCase {
            name: "unobstructed",
            start: Vec3I32::default(),
            end: destination,
            expected: unobstructed(destination),
        },
    );

    let stationary = Vec3I32 {
        x: 5 << 12,
        y: 9 << 12,
        z: -2 << 12,
    };
    assert_golden(
        &empty,
        GoldenTraceCase {
            name: "zero-length open",
            start: stationary,
            end: stationary,
            expected: unobstructed(stationary),
        },
    );
}

#[test]
fn oracle_pins_axial_hits_in_quake_z_up_space() {
    let axes = [
        (
            "x",
            Vec3I16 {
                x: 4096,
                y: 0,
                z: 0,
            },
            0,
            Vec3I32 {
                x: 4096,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: -4096,
                y: 0,
                z: 0,
            },
            Vec3I32 { x: 128, y: 0, z: 0 },
        ),
        (
            "y",
            Vec3I16 {
                x: 0,
                y: 4096,
                z: 0,
            },
            1,
            Vec3I32 {
                x: 0,
                y: 4096,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -4096,
                z: 0,
            },
            Vec3I32 { x: 0, y: 128, z: 0 },
        ),
        (
            "z-up",
            Vec3I16 {
                x: 0,
                y: 0,
                z: 4096,
            },
            2,
            Vec3I32 {
                x: 0,
                y: 0,
                z: 4096,
            },
            Vec3I32 {
                x: 0,
                y: 0,
                z: -4096,
            },
            Vec3I32 { x: 0, y: 0, z: 128 },
        ),
    ];

    for (name, normal, kind, start, end, stopped) in axes {
        let hull = one_plane_hull(normal, kind);
        assert_golden(
            &hull,
            GoldenTraceCase {
                name,
                start,
                end,
                expected: Trace {
                    all_solid: false,
                    start_solid: false,
                    in_open: true,
                    in_water: false,
                    fraction: 1984,
                    end: stopped,
                    normal,
                    plane_distance: 0,
                },
            },
        );
    }
}

#[test]
fn oracle_pins_start_solid_and_all_solid_flags() {
    let hull = one_plane_hull(
        Vec3I16 {
            x: 4096,
            y: 0,
            z: 0,
        },
        0,
    );
    let solid_start = Vec3I32 {
        x: -4096,
        y: 0,
        z: 0,
    };
    let deeper_solid = Vec3I32 {
        x: -8192,
        y: 0,
        z: 0,
    };
    assert_golden(
        &hull,
        GoldenTraceCase {
            name: "all solid",
            start: solid_start,
            end: deeper_solid,
            expected: Trace {
                all_solid: true,
                start_solid: true,
                in_open: false,
                in_water: false,
                fraction: Q12_ONE,
                end: deeper_solid,
                normal: Vec3I16::default(),
                plane_distance: 0,
            },
        },
    );

    let open_end = Vec3I32 {
        x: 4096,
        y: 0,
        z: 0,
    };
    assert_golden(
        &hull,
        GoldenTraceCase {
            name: "start solid then leave",
            start: solid_start,
            end: open_end,
            expected: Trace {
                all_solid: false,
                start_solid: true,
                in_open: true,
                in_water: false,
                fraction: Q12_ONE,
                end: open_end,
                normal: Vec3I16::default(),
                plane_distance: 0,
            },
        },
    );
}

#[test]
fn oracle_pins_near_plane_epsilon() {
    let hull = one_plane_hull(
        Vec3I16 {
            x: 4096,
            y: 0,
            z: 0,
        },
        0,
    );
    let start = Vec3I32 {
        x: PLANE_EPSILON_Q12 - 1,
        y: 0,
        z: 0,
    };
    assert_golden(
        &hull,
        GoldenTraceCase {
            name: "inside epsilon",
            start,
            end: Vec3I32 {
                x: -4096,
                y: 0,
                z: 0,
            },
            expected: Trace {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 0,
                end: start,
                normal: Vec3I16 {
                    x: 4096,
                    y: 0,
                    z: 0,
                },
                plane_distance: 0,
            },
        },
    );
}

#[test]
fn oracle_pins_shifted_plane_and_reverse_side_orientation() {
    let distance = 3 << 12;
    let hull = OracleHull::new(
        &[PlaneSpec {
            normal: Vec3I16 {
                x: 4096,
                y: 0,
                z: 0,
            },
            distance,
            kind: 0,
        }],
        &[NodeSpec {
            plane: 0,
            children: [CONTENTS_SOLID, CONTENTS_EMPTY],
        }],
        0,
    );
    assert_golden(
        &hull,
        GoldenTraceCase {
            name: "reverse side of shifted plane",
            start: Vec3I32 {
                x: 2 << 12,
                y: 0,
                z: 0,
            },
            end: Vec3I32 {
                x: 4 << 12,
                y: 0,
                z: 0,
            },
            expected: Trace {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 1984,
                end: Vec3I32 {
                    x: distance - PLANE_EPSILON_Q12,
                    y: 0,
                    z: 0,
                },
                normal: Vec3I16 {
                    x: -4096,
                    y: 0,
                    z: 0,
                },
                plane_distance: -distance,
            },
        },
    );
}

#[test]
fn oracle_pins_non_axial_fixed_point_rounding() {
    let diagonal = Vec3I16 {
        x: 2896,
        y: 2896,
        z: 0,
    };
    let hull = one_plane_hull(diagonal, 3);
    assert_golden(
        &hull,
        GoldenTraceCase {
            name: "non-axial diagonal",
            start: Vec3I32 {
                x: 4096,
                y: 4096,
                z: 2048,
            },
            end: Vec3I32 {
                x: -4096,
                y: -4096,
                z: 2048,
            },
            expected: Trace {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 2002,
                // The shared high-precision contact solve lands on the
                // epsilon contour instead of re-interpolating the Q0.12
                // fraction and rounding both coordinates up to 92.
                end: Vec3I32 {
                    x: 91,
                    y: 91,
                    z: 2048,
                },
                normal: diagonal,
                plane_distance: 0,
            },
        },
    );
}

#[test]
fn oracle_pins_bsp_root_as_the_deterministic_tie_breaker() {
    let x = plane(
        Vec3I16 {
            x: 4096,
            y: 0,
            z: 0,
        },
        0,
    );
    let y = plane(
        Vec3I16 {
            x: 0,
            y: 4096,
            z: 0,
        },
        1,
    );
    let root_x = OracleHull::new(
        &[x, y],
        &[
            NodeSpec {
                plane: 0,
                children: [1, CONTENTS_SOLID],
            },
            NodeSpec {
                plane: 1,
                children: [CONTENTS_EMPTY, CONTENTS_SOLID],
            },
        ],
        0,
    );
    let root_y = OracleHull::new(
        &[x, y],
        &[
            NodeSpec {
                plane: 1,
                children: [1, CONTENTS_SOLID],
            },
            NodeSpec {
                plane: 0,
                children: [CONTENTS_EMPTY, CONTENTS_SOLID],
            },
        ],
        0,
    );
    let start = Vec3I32 {
        x: 4096,
        y: 4096,
        z: 0,
    };
    let end = Vec3I32 {
        x: -4096,
        y: -4096,
        z: 0,
    };

    let x_first = root_x.trace(start, end).unwrap();
    let y_first = root_y.trace(start, end).unwrap();
    assert_eq!(root_x.shared_trace(start, end), Some(x_first));
    assert_eq!(root_y.shared_trace(start, end), Some(y_first));
    assert_eq!(x_first.fraction, 1984);
    assert_eq!(
        x_first.end,
        Vec3I32 {
            x: 128,
            y: 128,
            z: 0
        }
    );
    assert_eq!(
        x_first.normal,
        Vec3I16 {
            x: 4096,
            y: 0,
            z: 0
        }
    );
    assert_eq!(y_first.fraction, 1984);
    assert_eq!(
        y_first.end,
        Vec3I32 {
            x: 128,
            y: 128,
            z: 0
        }
    );
    assert_eq!(
        y_first.normal,
        Vec3I16 {
            x: 0,
            y: 4096,
            z: 0
        }
    );

    for _ in 0..64 {
        assert_eq!(root_x.trace(start, end), Some(x_first));
        assert_eq!(root_y.trace(start, end), Some(y_first));
        assert_eq!(root_x.shared_trace(start, end), Some(x_first));
        assert_eq!(root_y.shared_trace(start, end), Some(y_first));
    }
}

#[test]
fn oracle_pins_quake_contents_classification() {
    let point = Vec3I32::default();
    let cases = [
        (CONTENTS_EMPTY, false, true, false, false),
        (CONTENTS_SOLID, true, false, true, false),
        (CONTENTS_WATER, false, false, false, true),
        (CONTENTS_SLIME, false, false, false, true),
        (CONTENTS_LAVA, false, false, false, true),
        (CONTENTS_SKY, false, false, false, true),
    ];

    for (contents, all_solid, in_open, start_solid, in_water) in cases {
        let hull = OracleHull::new(&[], &[], contents);
        assert_eq!(hull.point_contents(point), Some(contents));
        assert_eq!(hull.shared_point_contents(point), Some(contents));
        assert_eq!(
            hull.trace(point, point),
            Some(Trace {
                all_solid,
                start_solid,
                in_open,
                in_water,
                fraction: Q12_ONE,
                end: point,
                normal: Vec3I16::default(),
                plane_distance: 0,
            }),
            "contents {contents}"
        );
        assert_eq!(
            hull.shared_trace(point, point),
            hull.trace(point, point),
            "shared contents {contents}"
        );
    }

    let water_to_solid = OracleHull::new(
        &[plane(
            Vec3I16 {
                x: 4096,
                y: 0,
                z: 0,
            },
            0,
        )],
        &[NodeSpec {
            plane: 0,
            children: [CONTENTS_WATER, CONTENTS_SOLID],
        }],
        0,
    );
    assert_eq!(
        water_to_solid
            .trace(
                Vec3I32 {
                    x: 4096,
                    y: 0,
                    z: 0,
                },
                Vec3I32 {
                    x: -4096,
                    y: 0,
                    z: 0,
                },
            )
            .unwrap(),
        Trace {
            all_solid: false,
            start_solid: false,
            in_open: false,
            in_water: true,
            fraction: 1984,
            end: Vec3I32 { x: 128, y: 0, z: 0 },
            normal: Vec3I16 {
                x: 4096,
                y: 0,
                z: 0,
            },
            plane_distance: 0,
        }
    );
}

#[test]
fn oracle_pins_invalid_structure_as_trace_failure() {
    let invalid_node = OracleHull::new(&[], &[], 0);
    assert_eq!(
        invalid_node.trace(Vec3I32::default(), Vec3I32::default()),
        None
    );
    assert_eq!(
        invalid_node.shared_trace(Vec3I32::default(), Vec3I32::default()),
        None
    );

    let invalid_plane = OracleHull::new(
        &[],
        &[NodeSpec {
            plane: 0,
            children: [CONTENTS_EMPTY, CONTENTS_SOLID],
        }],
        0,
    );
    assert_eq!(
        invalid_plane.trace(Vec3I32::default(), Vec3I32::default()),
        None
    );
    assert_eq!(
        invalid_plane.shared_trace(Vec3I32::default(), Vec3I32::default()),
        None
    );
}

#[test]
fn shared_oracle_pins_translated_and_rotated_quake_brushes() {
    let hull = one_plane_hull(
        Vec3I16 {
            x: Q12_ONE as i16,
            y: 0,
            z: 0,
        },
        0,
    );
    let origin = Vec3I32 {
        x: 10 * Q12_ONE,
        y: 20 * Q12_ONE,
        z: 30 * Q12_ONE,
    };
    let mut scratch = SharedTraceScratch::new();

    let translated = shared_transformed_trace(
        &hull,
        origin,
        IDENTITY_ROTATION_Q12,
        Vec3I32 {
            x: origin.x + Q12_ONE,
            y: origin.y,
            z: origin.z,
        },
        Vec3I32 {
            x: origin.x - Q12_ONE,
            y: origin.y,
            z: origin.z,
        },
        &mut scratch,
    )
    .unwrap();
    assert_eq!(translated.fraction, 1984);
    assert_eq!(
        translated.end,
        Vec3I32 {
            x: origin.x + 128,
            y: origin.y,
            z: origin.z,
        }
    );
    assert_eq!(
        translated.normal,
        Vec3I16 {
            x: Q12_ONE as i16,
            y: 0,
            z: 0,
        }
    );
    assert_eq!(translated.plane_distance, origin.x);

    let quake_z_yaw_90: RotationQ12 = [
        [0, -(Q12_ONE as i16), 0],
        [Q12_ONE as i16, 0, 0],
        [0, 0, Q12_ONE as i16],
    ];
    let rotated = shared_transformed_trace(
        &hull,
        origin,
        quake_z_yaw_90,
        Vec3I32 {
            x: origin.x,
            y: origin.y + Q12_ONE,
            z: origin.z,
        },
        Vec3I32 {
            x: origin.x,
            y: origin.y - Q12_ONE,
            z: origin.z,
        },
        &mut scratch,
    )
    .unwrap();
    assert_eq!(rotated.fraction, 1984);
    assert_eq!(
        rotated.end,
        Vec3I32 {
            x: origin.x,
            y: origin.y + 128,
            z: origin.z,
        }
    );
    assert_eq!(
        rotated.normal,
        Vec3I16 {
            x: 0,
            y: Q12_ONE as i16,
            z: 0,
        }
    );
    assert_eq!(rotated.plane_distance, origin.y);
}

#[test]
fn shared_oracle_preserves_output_on_failure_and_reuses_scratch() {
    let start = Vec3I32 {
        x: (TRACE_STACK_CAPACITY as i32 + 2) * Q12_ONE,
        y: 0,
        z: 0,
    };
    let end = Vec3I32 {
        x: -Q12_ONE,
        y: 0,
        z: 0,
    };
    let mut scratch = SharedTraceScratch::new();
    // Flag bytes that are neither zero nor one, including the 0xe7 the guest
    // was observed carrying. They are legal values in a byte-backed slot, and
    // the failure contract has to preserve them exactly like any other byte.
    let sentinel = SharedTrace {
        all_solid: TraceFlag::from_byte(0xe7),
        start_solid: TraceFlag::from_byte(0x01),
        in_open: TraceFlag::from_byte(0x00),
        in_water: TraceFlag::from_byte(0xff),
        fraction: 0x1122_3344,
        end: psx_bsp::Vec3I32 {
            x: 0x0102_0304,
            y: 0x1112_1314,
            z: 0x2122_2324,
        },
        normal: psx_bsp::Vec3I16 {
            x: 0x3132,
            y: 0x4142,
            z: 0x5152,
        },
        plane_distance: 0x6162_6364,
    };

    let invalid = OracleHull::new(&[], &[], 0);
    let adapter_sentinel = Trace {
        all_solid: sentinel.all_solid.is_set(),
        start_solid: sentinel.start_solid.is_set(),
        in_open: sentinel.in_open.is_set(),
        in_water: sentinel.in_water.is_set(),
        fraction: sentinel.fraction,
        end: psoxide_point_to_quake(sentinel.end),
        normal: psoxide_normal_to_quake(sentinel.normal),
        plane_distance: sentinel.plane_distance,
    };
    let mut adapter_output = adapter_sentinel;
    assert!(!invalid
        .collision_hull()
        .trace_into(&start, &end, &mut scratch, &mut adapter_output,));
    assert_eq!(adapter_output, adapter_sentinel);

    let mut invalid_output = sentinel;
    assert!(!invalid.shared_trace_with_scratch(start, end, &mut scratch, &mut invalid_output));
    assert_eq!(invalid_output, sentinel);

    let boundary = deep_crossing_hull(TRACE_STACK_CAPACITY);
    let mut boundary_output = SharedTrace::default();
    assert!(boundary.shared_trace_with_scratch(start, end, &mut scratch, &mut boundary_output,));
    assert_eq!(
        boundary_output.plane_distance,
        (TRACE_STACK_CAPACITY as i32 - 1) * Q12_ONE
    );

    let overflow = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
    let mut overflow_output = sentinel;
    assert!(!overflow.shared_trace_with_scratch(start, end, &mut scratch, &mut overflow_output,));
    assert_eq!(overflow_output, sentinel);

    let simple = one_plane_hull(
        Vec3I16 {
            x: Q12_ONE as i16,
            y: 0,
            z: 0,
        },
        0,
    );
    let simple_start = Vec3I32 {
        x: Q12_ONE,
        y: 0,
        z: 0,
    };
    let simple_end = Vec3I32 {
        x: 2 * Q12_ONE,
        y: 0,
        z: 0,
    };
    let mut reused = SharedTrace::default();
    assert!(simple.shared_trace_with_scratch(simple_start, simple_end, &mut scratch, &mut reused,));
    let mut fresh = SharedTrace::default();
    assert!(simple.shared_trace_with_scratch(
        simple_start,
        simple_end,
        &mut SharedTraceScratch::new(),
        &mut fresh,
    ));
    assert_eq!(reused, fresh);
}

#[test]
fn adapter_permutation_maps_quake_z_up_to_psoxide_y_up_and_back() {
    const QUAKE_X: Vec3I32 = Vec3I32 {
        x: 4096,
        y: 0,
        z: 0,
    };
    const QUAKE_Y: Vec3I32 = Vec3I32 {
        x: 0,
        y: 4096,
        z: 0,
    };
    const QUAKE_UP: Vec3I32 = Vec3I32 {
        x: 0,
        y: 0,
        z: 4096,
    };
    assert_eq!(
        quake_point_to_psoxide(QUAKE_X),
        psx_bsp::Vec3I32 {
            x: 4096,
            y: 0,
            z: 0,
        }
    );
    assert_eq!(
        quake_point_to_psoxide(QUAKE_Y),
        psx_bsp::Vec3I32 {
            x: 0,
            y: 0,
            z: 4096
        }
    );
    assert_eq!(
        quake_point_to_psoxide(QUAKE_UP),
        psx_bsp::Vec3I32 {
            x: 0,
            y: 4096,
            z: 0
        }
    );
    assert_eq!(
        quake_normal_to_psoxide(Vec3I16 {
            x: 0,
            y: 0,
            z: 4096
        }),
        psx_bsp::Vec3I16 {
            x: 0,
            y: 4096,
            z: 0
        }
    );

    let q20_12 = Vec3I32 {
        x: 123_456_789,
        y: -98_765_432,
        z: 17_203,
    };
    assert_eq!(
        psoxide_point_to_quake(quake_point_to_psoxide(q20_12)),
        q20_12
    );

    let quake_yaw_90: RotationQ12 = [[0, -4096, 0], [4096, 0, 0], [0, 0, 4096]];
    let psoxide_yaw_90 = [[0, 0, -4096], [0, 4096, 0], [4096, 0, 0]];
    assert_eq!(quake_rotation_to_psoxide(quake_yaw_90), psoxide_yaw_90);
    assert_eq!(psoxide_rotation_to_quake(psoxide_yaw_90), quake_yaw_90);
}
