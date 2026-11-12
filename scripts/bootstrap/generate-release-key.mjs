#!/usr/bin/env node
// Release-side only. Never prints private key material or overwrites a key.
import { generateKeyPairSync, createHash } from 'node:crypto';
import { mkdir, writeFile, chmod } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
const args=process.argv.slice(2);
if(args.length!==2||args[0]!=='--output'||!args[1]) throw new Error('Usage: node generate-release-key.mjs --output NEW_PRIVATE_DIRECTORY');
const output=path.resolve(args[1]);
await mkdir(output,{mode:0o700}); // no recursive creation and no reuse
if(process.platform==='win32') {
  const script='$ErrorActionPreference="Stop"; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=New-Object System.Security.AccessControl.DirectorySecurity; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); $rule=New-Object System.Security.AccessControl.FileSystemAccessRule($sid,"FullControl","ContainerInherit,ObjectInherit","None","Allow"); $acl.AddAccessRule($rule); Set-Acl -LiteralPath $env:VESTA_NEW_KEY_DIRECTORY -AclObject $acl';
  const result=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-Command',script],{env:{...process.env,VESTA_NEW_KEY_DIRECTORY:output},encoding:'utf8',windowsHide:true});
  if(result.status!==0) throw new Error('Private key directory ACL setup failed; no key generated.');
} else await chmod(output,0o700);
const {privateKey,publicKey}=generateKeyPairSync('ed25519');
const privatePem=privateKey.export({type:'pkcs8',format:'pem'});
const publicPem=publicKey.export({type:'spki',format:'pem'});
const raw=publicKey.export({type:'spki',format:'der'}).subarray(-32);
await writeFile(path.join(output,'release-ed25519.pk8.pem'),privatePem,{flag:'wx',mode:0o600});
await writeFile(path.join(output,'release-public.pem'),publicPem,{flag:'wx',mode:0o600});
await writeFile(path.join(output,'release-public.hex'),raw.toString('hex')+'\n',{flag:'wx',mode:0o600});
console.log(`Public release key: ${raw.toString('hex')}\nSHA-256 fingerprint: ${createHash('sha256').update(raw).digest('hex')}\nPrivate key saved in the new protected directory. Keep it off game hosts and out of Git.`);
