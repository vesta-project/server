//! Deliberately narrow Forge argument grammar. Never forward an unparsed @file.
use std::path::{Path};
pub(super) fn parse(game:&Path,file:&Path)->Result<Vec<String>, &'static str> {
 let fail="launch_arguments_unsupported";
 use std::io::Read;
 let opened=crate::files::native::import_open(file,false).map_err(|_|fail)?;
 let mut text=String::new();opened.take(1024*1024+1).read_to_string(&mut text).map_err(|_|fail)?;
 if text.len()>1024*1024 {return Err(fail);}
 let mut tokens=Vec::new();let mut token=String::new();let mut quote=None;
 for line in text.lines() {
  if line.trim_start().starts_with('#'){continue;}
  for c in line.chars().chain([' ']) {
   if c=='\\' || c=='\0' {return Err(fail);}
   if Some(c)==quote {quote=None;} else if quote.is_some(){token.push(c);}
   else if c=='\'' || c=='"' {quote=Some(c);}
   else if c.is_whitespace(){if !token.is_empty(){tokens.push(std::mem::take(&mut token));}}
   else {token.push(c);}
  }
 }
 if quote.is_some(){return Err(fail);}
 let root=game.canonicalize().map_err(|_|fail)?;
 let paths=|value:&str|->Result<String,&'static str>{
  let separator=if cfg!(windows){';'}else{':'};
  value.split(separator).map(|p| {
   if p.is_empty() || p.contains(['\\',':']) || Path::new(p).is_absolute() || p.split('/').any(|p|matches!(p,".."|"")) {return Err(fail);}
   let path=game.join(p);let actual=path.canonicalize().map_err(|_|fail)?;
   if !actual.starts_with(&root){return Err(fail);}
   Ok(path.to_string_lossy().into_owned())
  }).collect::<Result<Vec<_>,_>>().map(|v|v.join(&separator.to_string()))
 };
 let mut out=Vec::new();let mut i=0;let mut main=false;
 while i<tokens.len(){
  let t=&tokens[i];i+=1;
  match t.as_str(){
   "-p"|"--module-path"|"-cp"|"-classpath"|"--class-path"=>{let v=tokens.get(i).ok_or(fail)?;i+=1;out.extend([t.clone(),paths(v)?]);}
   "--add-modules"|"--add-opens"|"--add-exports"=>{
    let v=tokens.get(i).ok_or(fail)?;i+=1;
    if !v.chars().all(|c|c.is_ascii_alphanumeric()||"._/=,-".contains(c)){return Err(fail);}out.extend([t.clone(),v.clone()]);
   }
   "cpw.mods.bootstraplauncher.BootstrapLauncher"|"net.minecraftforge.bootstrap.ForgeBootstrap"|"net.neoforged.serverstarterjar.Main"=>{if main{return Err(fail);}main=true;out.push(t.clone());}
   "--launchTarget"|"--fml.forgeVersion"|"--fml.mcVersion"|"--fml.forgeGroup"|"--fml.mcpVersion"|"--fml.neoForgeVersion"=>{
    let v=tokens.get(i).ok_or(fail)?;i+=1;if !v.chars().all(|c|c.is_ascii_alphanumeric()||"._-".contains(c)){return Err(fail);}out.extend([t.clone(),v.clone()]);
   }
   _=>{
    let (k,v)=t.split_once('=').ok_or(fail)?;
    match k {
     "-DlibraryDirectory"|"-DlegacyClassPath"|"-Djava.library.path"=>out.push(format!("{k}={}",paths(v)?)),
     "-DignoreList"|"-DmergeModules"|"-Djava.net.preferIPv6Addresses"=>{
      if !v.chars().all(|c|c.is_ascii_alphanumeric()||"._,-".contains(c)){return Err(fail);}out.push(t.clone());
     }
     _=>return Err(fail)
    }
   }
  }
 }
 if !main{return Err(fail);}Ok(out)
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn rejects_hostile_arguments(){let t=tempfile::tempdir().unwrap();let f=t.path().join("args");
 for s in ["-javaagent:x", "-agentpath:/tmp/x", "-agentlib:x", "@/tmp/x", "-cp /tmp/x", "-Dunknown=x", "-cp ../x"] {
 std::fs::write(&f,s).unwrap();assert_eq!(parse(t.path(),&f).unwrap_err(),"launch_arguments_unsupported");
 }
 std::fs::write(&f,"cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget forgeserver").unwrap();assert!(parse(t.path(),&f).is_ok());
 }
}
