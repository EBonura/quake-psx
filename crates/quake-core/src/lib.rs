#![feature(optimize_attribute)]
#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

//! Platform-independent Quake simulation code.
//!
//! PSoXide supplies shared fixed-point arithmetic and hardware-independent
//! primitives. This crate owns only Quake's BSP and gameplay semantics.

pub mod ambient;
pub mod body;
pub mod bsp_axis_adapter;
pub mod collision;
pub mod combat;
pub mod cover;
pub mod door;
pub mod effects;
pub mod electrode;
pub mod hud;
pub mod level;
pub mod level_session;
pub mod lightstyle;
pub mod liquid;
pub mod loading;
pub mod menu;
pub mod monster;
pub mod movement;
pub mod mover;
pub mod pose;
pub mod push;
pub mod screenblend;
pub mod save;
pub mod secrets;
pub mod sky;
pub mod spatial;
pub mod survival;
pub mod targets;
pub mod teleport;
pub mod text;
pub mod train;
pub mod traps;
pub mod trigger;
pub mod view;
pub mod view_model;
pub mod waterwarp;
