use quake_formats::{LumpKind, PSB5_MAGIC};

use super::{
    cook_entities, cook_geometry_and_models, cook_monolithic_sounds_for_validation, cook_sounds,
    merge_sound_banks_for_validation, Bsp, CookError, CookedEntities, CookedGlobalSounds,
    CookedModels, GeometryLumps, PakArchive, SkyEncoding, SoundCookStats,
};

#[derive(Clone, Copy, Debug)]
pub struct MapCookConfig<'a> {
    pub entity_map: &'a str,
    pub model_map: &'a str,
    pub sound_map: &'a str,
    pub resource_list: &'a str,
    pub model_props: &'a str,
    pub global_sounds: &'a CookedGlobalSounds,
    pub sky: SkyEncoding,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedMap {
    pub psb: Vec<u8>,
    pub model_count: usize,
    pub sound_stats: SoundCookStats,
    pub entity_count: usize,
}

/// Cook one BSP29 map into the complete PSB consumed by the PS1 runtime.
pub fn cook_map(
    pak: &PakArchive<'_>,
    bsp: &Bsp<'_>,
    config: MapCookConfig<'_>,
) -> Result<CookedMap, CookError> {
    let entities = cook_entities(
        bsp,
        config.entity_map,
        config.model_map,
        config.resource_list,
    )?;
    let (geometry, models) = cook_geometry_and_models(
        bsp,
        pak,
        &entities,
        config.model_map,
        config.resource_list,
        config.model_props,
        config.sky,
    )?;
    let sounds = cook_sounds(
        pak,
        &entities,
        config.sound_map,
        config.resource_list,
        config.global_sounds,
    )?;
    let monolithic = cook_monolithic_sounds_for_validation(
        pak,
        &entities,
        config.sound_map,
        config.resource_list,
    )?;
    let reconstructed = merge_sound_banks_for_validation(config.global_sounds, &sounds)?;
    if reconstructed != monolithic {
        return Err(CookError::new(
            "global plus local sound banks do not roundtrip to the monolithic selection",
        ));
    }
    let psb = assemble_psb(&geometry, &models, &entities, &sounds.data)?;
    Ok(CookedMap {
        psb,
        model_count: models.stats.model_count,
        sound_stats: sounds.stats,
        entity_count: entities.entities.len() / 50,
    })
}

fn assemble_psb(
    geometry: &GeometryLumps,
    models: &CookedModels,
    entities: &CookedEntities,
    sounds: &[u8],
) -> Result<Vec<u8>, CookError> {
    let lumps: [(LumpKind, &[u8]); 15] = [
        (LumpKind::TextureData, &geometry.texture_data),
        (LumpKind::SoundData, sounds),
        (LumpKind::ModelData, &models.data),
        (LumpKind::Vertices, &geometry.vertices),
        (LumpKind::Planes, &geometry.planes),
        (LumpKind::TextureInfo, &geometry.texture_info),
        (LumpKind::Faces, &geometry.faces),
        (LumpKind::MarkSurfaces, &geometry.mark_surfaces),
        (LumpKind::Visibility, &geometry.visibility),
        (LumpKind::Leaves, &geometry.leaves),
        (LumpKind::Nodes, &geometry.nodes),
        (LumpKind::ClipNodes, &geometry.clip_nodes),
        (LumpKind::Models, &geometry.models),
        (LumpKind::Strings, &entities.strings),
        (LumpKind::Entities, &entities.entities),
    ];
    let payload_bytes = lumps.iter().try_fold(0usize, |total, (_, bytes)| {
        total
            .checked_add(8)
            .and_then(|total| total.checked_add(bytes.len()))
            .ok_or_else(|| CookError::new("PSB size overflow"))
    })?;
    let mut output = Vec::with_capacity(4 + payload_bytes);
    output.extend_from_slice(&PSB5_MAGIC.to_le_bytes());
    for (kind, bytes) in lumps {
        let len = i32::try_from(bytes.len())
            .map_err(|_| CookError::new(format!("{kind:?} lump exceeds i32")))?;
        output.extend_from_slice(&(kind as i32).to_le_bytes());
        output.extend_from_slice(&len.to_le_bytes());
        output.extend_from_slice(bytes);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_formats::{PsbIndex, SliceReader};

    #[test]
    fn empty_lump_set_assembles_a_valid_index() {
        let geometry = GeometryLumps::default();
        let models = CookedModels::default();
        let entities = CookedEntities::default();
        let bytes = assemble_psb(&geometry, &models, &entities, &[]).unwrap();
        let mut reader = SliceReader::new(&bytes);
        let index = PsbIndex::read(&mut reader).unwrap();
        assert_eq!(index.version(), quake_formats::PsbVersion::IndexedV5);
        assert_eq!(index.lump(LumpKind::TextureData).len, 0);
        assert_eq!(index.lump(LumpKind::Entities).len, 0);
    }
}
