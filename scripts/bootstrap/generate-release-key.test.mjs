import assert from 'node:assert/strict';
import {createPrivateKey,createPublicKey,sign,verify} from 'node:crypto';
import {mkdtemp,readFile,rm,stat} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import os from 'node:os';import path from 'node:path';import test from 'node:test';
test('key generation protects fresh output, exports the matching public pin and refuses overwrite',async t=>{
  const root=await mkdtemp(path.join(os.tmpdir(),'vesta-key-test-'));t.after(()=>rm(root,{recursive:true,force:true}));
  const output=path.join(root,'keys');const script=fileURLToPath(new URL('./generate-release-key.mjs',import.meta.url));
  const args=[script,'--output',output];const run=()=>spawnSync(process.execPath,args,{encoding:'utf8'});
  const first=run();assert.equal(first.status,0,first.stderr);
  assert(!first.stdout.includes('PRIVATE KEY'));assert(!first.stderr.includes('PRIVATE KEY'));
  const pem=await readFile(path.join(output,'release-ed25519.pk8.pem'));
  const privateKey=createPrivateKey(pem);const publicKey=createPublicKey(await readFile(path.join(output,'release-public.pem')));
  const pin=(await readFile(path.join(output,'release-public.hex'),'utf8')).trim();
  assert.equal(publicKey.export({type:'spki',format:'der'}).subarray(-32).toString('hex'),pin);
  assert.equal(verify(null,Buffer.from('fixture'),publicKey,sign(null,Buffer.from('fixture'),privateKey)),true);
  assert.notEqual(run().status,0);assert.deepEqual(await readFile(path.join(output,'release-ed25519.pk8.pem')),pem);
  if(process.platform!=='win32'){assert.equal((await stat(output)).mode&0o777,0o700);assert.equal((await stat(path.join(output,'release-ed25519.pk8.pem'))).mode&0o777,0o600);}
});
