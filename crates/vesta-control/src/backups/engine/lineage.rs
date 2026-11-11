use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPoint {
    pub id: String,
    pub time: String,
    pub profile: Option<String>,
    pub safety: bool,
    pub parent_id: Option<String>,
    pub label: Option<String>,
    pub kept: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupHistory {
    pub head: Option<String>,
    pub points: Vec<BackupPoint>,
}
fn present_label<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointUpdate {
    #[serde(
        default,
        deserialize_with = "present_label",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<bool>,
}
fn promoted(r: &TagRecord) -> bool {
    r.tags
        .iter()
        .any(|t| t.starts_with(PROFILE_PREFIX) || t.starts_with(SAFETY_PREFIX))
}
impl Engine {
    pub fn head(&self) -> Result<Option<String>> {
        match self.store.get("head.json") {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| EngineError::Corrupt),
            Err(StoreError::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub(super) fn set_head(&self, id: Option<&str>) -> Result<()> {
        self.store.put(
            "head.json",
            &serde_json::to_vec(&id).map_err(|_| EngineError::Failed)?,
        )?;
        Ok(())
    }
    fn write_record(&self, id: &str, record: &TagRecord) -> Result<()> {
        self.store.put(
            &format!("tags/{id}"),
            &serde_json::to_vec(record).map_err(|_| EngineError::Failed)?,
        )?;
        Ok(())
    }
    pub fn history(&self) -> Result<BackupHistory> {
        self.require_initialized()?;
        let records = self.all()?;
        let listed: HashSet<&str> = records
            .iter()
            .filter(|(_, r)| promoted(r))
            .map(|(id, _)| id.as_str())
            .collect();
        let head = self.head()?.filter(|id| listed.contains(id.as_str()));
        let points = records
            .iter()
            .rev()
            .filter(|(_, r)| promoted(r))
            .map(|(id, r)| BackupPoint {
                id: id.clone(),
                time: rfc3339(r.time_ms),
                // A safety point belongs to the profile whose restore took it; restoring it
                // again goes through that profile.
                profile: r.tags.iter().find_map(|t| {
                    t.strip_prefix(PROFILE_PREFIX)
                        .or_else(|| t.strip_prefix(SAFETY_PREFIX))
                        .map(str::to_owned)
                }),
                safety: r.tags.iter().any(|t| t.starts_with(SAFETY_PREFIX)),
                parent_id: r
                    .parent_id
                    .clone()
                    .filter(|id| listed.contains(id.as_str())),
                label: r.label.clone(),
                kept: r.kept,
            })
            .collect();
        Ok(BackupHistory { head, points })
    }
    pub fn point_update(&self, id: &str, update: &PointUpdate) -> Result<BackupPoint> {
        let mut record = self
            .all()?
            .into_iter()
            .find(|(key, r)| key == id && promoted(r))
            .map(|(_, r)| r)
            .ok_or(EngineError::Missing)?;
        if let Some(label) = &update.label {
            record.label = match label {
                None => None,
                Some(label) => {
                    let label = label.trim();
                    if !(1..=60).contains(&label.chars().count()) {
                        return Err(EngineError::Invalid);
                    }
                    Some(label.to_owned())
                }
            };
        }
        if let Some(kept) = update.kept {
            record.kept = kept;
        }
        self.write_record(id, &record)?;
        self.history()?
            .points
            .into_iter()
            .find(|p| p.id == id)
            .ok_or(EngineError::Missing)
    }
    pub(super) fn reparent_before_remove(&self, id: &str) -> Result<()> {
        let records = self.all()?;
        let Some((_, removed)) = records.iter().find(|(key, _)| key == id) else {
            return Ok(());
        };
        if removed.kept {
            return Err(EngineError::Invalid);
        }
        for (child, record) in &records {
            if record.parent_id.as_deref() == Some(id) {
                let mut record = record.clone();
                record.parent_id.clone_from(&removed.parent_id);
                self.write_record(child, &record)?;
            }
        }
        if self.head()?.as_deref() == Some(id) {
            self.set_head(removed.parent_id.as_deref())?;
        }
        Ok(())
    }
}
