use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::Compiler;
use crate::storage::{removePath, replaceDirectory, updateMasterIndex, writeJson, writeJsonPlain};
impl Compiler {
    pub(crate) fn writeOutputs(&self, output_root: &Path) -> Result<()> {
        fs::create_dir_all(output_root)
            .with_context(|| format!("create {}", output_root.display()))?;
        let dataset_root = output_root.join(&self.dataset_version);
        let staging_root = output_root.join(format!(
            ".{}.tmp-{}",
            self.dataset_version,
            std::process::id()
        ));
        let backup_root = output_root.join(format!(
            ".{}.old-{}",
            self.dataset_version,
            std::process::id()
        ));

        removePath(&staging_root)?;
        fs::create_dir_all(staging_root.join("global"))?;
        fs::create_dir_all(staging_root.join("characters"))?;

        writeJson(&staging_root.join("global/global.json"), &self.globalJson())?;

        let mut character_ids: Vec<u32> = self.characters.keys().copied().collect();
        character_ids.sort_unstable();
        for character_id in character_ids {
            let character = self
                .characters
                .get(&character_id)
                .expect("character key exists");
            writeJson(
                &staging_root
                    .join("characters")
                    .join(format!("{character_id}.json")),
                &self.characterJson(character_id, character),
            )?;
        }

        let index = self.indexJson();
        writeJsonPlain(&staging_root.join("index.json"), &index)?;
        replaceDirectory(&staging_root, &dataset_root, &backup_root)?;
        updateMasterIndex(
            output_root,
            &self.dataset_version,
            &self.dataset_name,
            &self.generated_at,
            index,
        )?;

        Ok(())
    }
}
