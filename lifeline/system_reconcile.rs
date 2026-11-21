//! Read-only reconciliation between the protected object store and catalog.
//!
//! Reconciliation never deletes objects or repairs catalog records. Missing or
//! corrupt objects referenced by the catalog are fatal; unreferenced objects
//! and interrupted temporary files are reported for a later, explicit cleanup
//! policy.

#![allow(dead_code)]

use std::{
    collections::HashSet,
    path::Path,
};

use super::{system_catalog, system_store};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub catalog_records: usize,
    pub verified_objects: usize,
    pub orphaned_object_ids: Vec<String>,
    pub interrupted_temporary_ids: Vec<String>,
}

/// Verify that every catalog record points to an intact object with the
/// expected content length. Report unreferenced objects without deleting them.
pub fn reconcile(storage_root: &Path) -> Result<ReconciliationReport, String> {
    let records = system_catalog::read_all(storage_root)?;
    let inventory = system_store::inventory(storage_root)?;
    let mut referenced = HashSet::with_capacity(records.len());

    for record in &records {
        if !referenced.insert(record.object_id.as_str()) {
            return Err(format!(
                "system recovery catalog contains duplicate object ID {}",
                record.object_id
            ));
        }

        let bytes = system_store::get(storage_root, &record.object_id)
            .map_err(|error| {
                format!(
                    "catalog references missing or invalid recovery object {}: {error}",
                    record.object_id
                )
            })?;
        if bytes.len() as u64 != record.original_size {
            return Err(format!(
                "recovery object {} length does not match its catalog record",
                record.object_id
            ));
        }
    }

    let mut orphaned_object_ids = inventory.object_ids
        .into_iter()
        .filter(|object_id| !referenced.contains(object_id.as_str()))
        .collect::<Vec<_>>();
    orphaned_object_ids.sort();

    Ok(ReconciliationReport {
        catalog_records: records.len(),
        verified_objects: records.len(),
        orphaned_object_ids,
        interrupted_temporary_ids: inventory.temporary_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
    };

    fn temporary_store(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lifeline-reconcile-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn verifies_catalog_objects_and_reports_orphans_without_deleting_them() {
        let root = temporary_store("orphan");
        let source = root.join("source.conf");
        fs::write(&source, b"catalogued state").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let referenced = system_store::put(&root, b"catalogued state").unwrap();
        system_catalog::append(&root, referenced.as_str(), &source, &metadata).unwrap();

        let orphan = system_store::put(&root, b"unreferenced evidence").unwrap();
        let report = reconcile(&root).unwrap();

        assert_eq!(report.catalog_records, 1);
        assert_eq!(report.verified_objects, 1);
        assert_eq!(report.orphaned_object_ids, vec![orphan.as_str().to_string()]);
        assert_eq!(system_store::get(&root, orphan.as_str()).unwrap(), b"unreferenced evidence");
        assert!(system_store::get(&root, referenced.as_str()).is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fails_when_a_catalog_object_is_missing() {
        let root = temporary_store("missing");
        let source = root.join("source.conf");
        fs::write(&source, b"catalogued state").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let object = system_store::put(&root, b"catalogued state").unwrap();
        system_catalog::append(&root, object.as_str(), &source, &metadata).unwrap();
        fs::remove_file(root.join("objects").join(object.as_str())).unwrap();

        assert!(reconcile(&root).unwrap_err().contains("missing or invalid"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_interrupted_temporary_objects_without_removing_them() {
        let root = temporary_store("temporary");
        system_store::initialize(&root).unwrap();
        let temporary_id = "0123456789abcdef0123456789abcdef";
        let temporary_path = root.join("objects").join(format!(".tmp-{temporary_id}"));
        fs::write(&temporary_path, b"incomplete write").unwrap();
        fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600)).unwrap();

        let report = reconcile(&root).unwrap();
        assert_eq!(report.interrupted_temporary_ids, vec![temporary_id.to_string()]);
        assert!(temporary_path.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_duplicate_catalog_object_references() {
        let root = temporary_store("duplicate");
        let source = root.join("source.conf");
        fs::write(&source, b"state").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let object = system_store::put(&root, b"state").unwrap();
        system_catalog::append(&root, object.as_str(), &source, &metadata).unwrap();

        // Simulate a duplicate complete record in a damaged or older catalog.
        let catalog = root.join("catalog.log");
        let line = fs::read(&catalog).unwrap();
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new().append(true).open(&catalog).unwrap();
        file.write_all(&line).unwrap();
        file.sync_all().unwrap();

        assert!(reconcile(&root).unwrap_err().contains("duplicate object ID"));
        fs::remove_dir_all(root).unwrap();
    }
}
