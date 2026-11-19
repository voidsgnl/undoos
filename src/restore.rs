//! Recovery operations append compensating events; original history is immutable.
use std::fs;
use std::path::Path;
use crate::journal::{self,Entry};
use crate::snapshot;

fn apply_snapshot(name:&str,destination:&str)->Result<(),String>{
    let dest=Path::new(destination);
    if let Some(parent)=dest.parent(){fs::create_dir_all(parent).map_err(|e|format!("cannot create {}: {e}",parent.display()))?;}
    if name.is_empty(){
        if dest.exists(){fs::remove_file(dest).map_err(|e|format!("cannot remove {}: {e}",dest.display()))?;}
    }else{
        let src=snapshot::path_for(name);
        if !src.is_file(){return Err(format!("recovery copy is missing: {}",src.display()));}
        fs::copy(&src,dest).map_err(|e|format!("cannot restore {}: {e}",destination))?;
    }
    Ok(())
}
/// Restore a saved file and append a record of the state replaced.
pub fn undo_entry(entry:&Entry)->Result<(),String>{
    if entry.kind == "move" {
        let old = Path::new(&entry.path);
        let new = Path::new(&entry.snapshot);
        if old.exists() { return Err(format!("cannot undo move: {} already exists", old.display())); }
        if !new.exists() { return Err(format!("cannot undo move: {} is missing", new.display())); }
        let id = journal::next_id()?;
        if let Some(parent) = old.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        fs::rename(new, old).map_err(|e| format!("cannot move {} back to {}: {e}", new.display(), old.display()))?;
        if let Err(err) = journal::append(id, "restore", &entry.id.to_string(), "") {
            let _ = fs::rename(old, new);
            return Err(err);
        }
        return Ok(());
    }
    if entry.kind!="snap"{return Ok(());}
    let dest=Path::new(&entry.path); let id=journal::next_id()?;
    let backup=if dest.is_file(){snapshot::store(id,dest)?}else{String::new()};
    if let Err(err)=apply_snapshot(&entry.snapshot,&entry.path){if !backup.is_empty(){snapshot::remove(&backup);}return Err(err);}
    if let Err(err)=journal::append(id,"restore",&entry.id.to_string(),&backup){
        let _=apply_snapshot(&backup,&entry.path); if !backup.is_empty(){snapshot::remove(&backup);} return Err(err);
    }
    Ok(())
}
pub fn undo_last()->Result<Option<Entry>,String>{
    let e=match journal::active_snapshots()?.into_iter().max_by_key(|e|e.id){Some(e)=>e,None=>return Ok(None)};
    undo_entry(&e)?; Ok(Some(e))
}
/// Reapply the state which existed immediately before the latest undo.
pub fn redo_last()->Result<Option<Entry>,String>{
    let restore=match journal::pending_redo()?{Some(e)=>e,None=>return Ok(None)};
    let original=journal::read_all()?.into_iter().find(|e|(e.kind=="snap"||e.kind=="move")&&e.id.to_string()==restore.path)
        .ok_or_else(||format!("original entry #{} is missing",restore.path))?;
    let id=journal::next_id()?;
    if original.kind == "move" {
        let old = Path::new(&original.path);
        let new = Path::new(&original.snapshot);
        if new.exists() { return Err(format!("cannot redo move: {} already exists", new.display())); }
        if !old.exists() { return Err(format!("cannot redo move: {} is missing", old.display())); }
        if let Some(parent) = new.parent() { fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?; }
        fs::rename(old, new).map_err(|e| format!("cannot move {} to {}: {e}", old.display(), new.display()))?;
        if let Err(err) = journal::append(id, "redo", &restore.id.to_string(), "") {
            let _ = fs::rename(new, old);
            return Err(err);
        }
        return Ok(Some(restore));
    }
    let dest=Path::new(&original.path);
    let backup=if dest.is_file(){snapshot::store(id,dest)?}else{String::new()};
    if let Err(err)=apply_snapshot(&restore.snapshot,&original.path){if !backup.is_empty(){snapshot::remove(&backup);}return Err(err);}
    if let Err(err)=journal::append(id,"redo",&restore.id.to_string(),&backup){
        let _=apply_snapshot(&backup,&original.path);if !backup.is_empty(){snapshot::remove(&backup);}return Err(err);
    }
    Ok(Some(restore))
}
/// Rewind active saved changes newer than id, newest first, without truncating history.
pub fn rewind_to(id:u64)->Result<usize,String>{
    let mut newer:Vec<Entry>=journal::active_snapshots()?.into_iter().chain(journal::active_moves()?.into_iter()).filter(|e|e.id>id).collect();
    newer.sort_by_key(|e|std::cmp::Reverse(e.id)); let mut done=0;
    for e in newer{undo_entry(&e)?;done+=1;} Ok(done)
}
