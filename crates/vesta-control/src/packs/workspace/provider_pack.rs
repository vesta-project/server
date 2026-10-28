use super::super::{PackMutation, PackOutcome, PackReleases, PreparedPack, UpdatePolicy};
use super::*;
use vesta_protocol::pack::catalogue::{ManifestMetadata, Platform, PlatformPack};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPackPublish {
    pub expected_revision: String,
    pub draft_revision: String,
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub required_update: bool,
    pub acknowledge_lower_version: bool,
}
impl PackWorkspace {
    /// Publishes only a catalogue pointer. Provider archives and files are never hosted.
    pub fn publish_provider_pack(
        &self,
        subject: &Subject,
        mc: &str,
        key: &str,
        input: &ProviderPackPublish,
        releases: &PackReleases,
    ) -> Result<PackOutcome> {
        let expected = revision(&input.expected_revision)?;
        let sha = digest(&serde_json::to_vec(input).map_err(|_| invalid())?);
        let make_request = |json| {
            PackMutation::workspace_publish(
                key.into(),
                expected,
                &sha,
                revision(&input.draft_revision)?,
                json,
                input.acknowledge_lower_version,
                None,
                input.required_update,
            )
        };
        if let Some(outcome) = releases.remote_outcome(subject, &make_request(String::new())?)? {
            return Ok(outcome);
        }
        let draft = self.read(subject, mc)?;
        if draft.revision != input.draft_revision {
            return Err(ReleaseError::Conflict);
        }
        let reference = PlatformPack::new(
            match input.provider.as_str() {
                "modrinth" => Platform::Modrinth,
                "curseforge" => Platform::Curseforge,
                _ => return Err(invalid()),
            },
            &input.project_id,
            &input.version_id,
            None,
        )
        .map_err(super::super::PackError::from)?;
        let plan = releases.plan(
            subject,
            expected,
            if input.required_update {
                UpdatePolicy::Required
            } else {
                UpdatePolicy::Optional
            },
        )?;
        let metadata = ManifestMetadata::new(
            &draft.manifest.name,
            &draft.manifest.display_version,
            plan.revision(),
            plan.minimum_revision(),
        )
        .map_err(super::super::PackError::from)?;
        let prepared = PreparedPack::platform(reference, metadata)?;
        let request = make_request(serde_json::to_string(&draft.manifest).map_err(|_| invalid())?)?;
        releases.publish_remote(subject, &plan, &prepared, &request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::workspace::tests::Fixture;
    #[test]
    fn pointer_publication_is_revisioned_required_and_never_hosts_provider_bytes() {
        let f = Fixture::new();
        let releases = PackReleases::open(&f.db).unwrap();
        let draft = f
            .workspace
            .write(
                &f.subject,
                "1.21.1",
                "prelink",
                &DraftWrite {
                    expected_revision: "0".into(),
                    manifest: f.manifest(),
                },
            )
            .unwrap();
        let input = ProviderPackPublish {
            expected_revision: "0".into(),
            draft_revision: draft.revision,
            provider: "modrinth".into(),
            project_id: "project".into(),
            version_id: "v1".into(),
            required_update: true,
            acknowledge_lower_version: false,
        };
        let result = f
            .workspace
            .publish_provider_pack(&f.subject, "1.21.1", "pointer", &input, &releases)
            .unwrap();
        assert_eq!(result.hosted_revision, 1);
        assert_eq!(result.minimum_revision, 1);
        let current = releases.current().unwrap().unwrap();
        assert_eq!(
            current.artifact().format(),
            crate::packs::ArtifactFormat::Catalogue
        );
        assert!(current.artifact().size() < 1024);
        let view = f.workspace.view(&f.subject, "1.21.1", &releases).unwrap();
        assert!(!view.pending_publication);
        assert!(!view.customizing_provider);
        assert_eq!(view.provider_pack.unwrap().version_id, "v1");
        let history = f.workspace.history(&f.subject).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].revision, "1");
        assert_eq!(
            history[0].published_by.as_deref(),
            Some(f.subject.principal.as_str())
        );
        assert!(history[0].required_update);
        assert_eq!(history[0].provider_version.as_deref(), Some("v1"));
        assert_eq!((history[0].entry_count, history[0].override_count), (Some(0), Some(0)));
        assert_eq!(
            f.workspace
                .publish_provider_pack(&f.subject, "1.21.1", "pointer", &input, &releases)
                .unwrap()
                .hosted_revision,
            1
        );
    }
}
