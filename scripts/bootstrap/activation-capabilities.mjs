// Probe the actual copied release components, never a source-tree version or caller assertion.
import { spawnSync } from 'node:child_process';
export function readActivationCapabilities(paths,target,execute=spawnSync){
  const observed=paths.map(file=>{
    const result=execute(file,['update','capabilities'],{encoding:'utf8',timeout:20000,maxBuffer:8192,windowsHide:true,env:{...process.env,VESTA_INSTALLATION_ROOT:'',VESTA_UPDATE_READINESS_NONCE:''}});
    if(result.error||result.status!==0)return null;
    let value;try{value=JSON.parse(result.stdout);}catch{throw new Error('Release component returned malformed update capabilities.');}
    const a=value?.activation;
    if(value.target!==target||!a||a.protocol!==1||!Number.isSafeInteger(a.state_schema_min)||a.state_schema_min<1||a.state_schema_min!==a.state_schema_max||!Number.isInteger(a.supervisor_ipc)||a.supervisor_ipc<1||a.supervisor_ipc>255||Object.keys(a).sort().join(',')!=='protocol,state_schema_max,state_schema_min,supervisor_ipc')throw new Error('Release component update capabilities are incompatible.');
    return {protocol:a.protocol,state_schema_min:a.state_schema_min,state_schema_max:a.state_schema_max,supervisor_ipc:a.supervisor_ipc};
  });
  if(observed.every(v=>v===null))return null; // Explicit legacy staging-only package.
  if(observed.some(v=>v===null)||observed.some(v=>JSON.stringify(v)!==JSON.stringify(observed[0])))throw new Error('Release components disagree about update compatibility.');
  return observed[0];
}
