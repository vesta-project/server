// Screenshot the compiled Rust frames, never a separately authored UI mockup.
import { chromium } from 'playwright-core';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';

const args=process.argv.slice(2);
const value=key=>{const i=args.indexOf(key);if(i<0||!args[i+1])throw new Error(`Missing ${key}`);return args[i+1];};
const browserPath=value('--browser');
const output=path.resolve(value('--output'));
const base=value('--url');
const url=new URL(base);
if(!['127.0.0.1','localhost'].includes(url.hostname))throw new Error('Capture only a local review server');
await mkdir(output,{recursive:false});
const states=await fetch(new URL('/states.json',url)).then(r=>{if(!r.ok)throw new Error('Frame export missing');return r.json();});
const browser=await chromium.launch({headless:true,executablePath:browserPath});
const consoleErrors=[];
try{
  const page=await browser.newPage({viewport:{width:1180,height:1000},deviceScaleFactor:1});
  page.on('pageerror',error=>consoleErrors.push(error.message));
  page.on('console',message=>{if(message.type()==='error')consoleErrors.push(message.text());});
  await page.goto(new URL('/?capture=1',url).href);
  await page.waitForFunction(()=>document.documentElement.dataset.ready==='true');
  const manifest=[];
  for(const state of states){
    if(!/^[a-z0-9-]+$/.test(state.id))throw new Error('Invalid frame id');
    await page.evaluate(id=>window.showState(id),state.id);
    await page.waitForFunction(id=>window.currentStateId===id&&document.documentElement.dataset.ready==='true',state.id);
    const file=state.id+'.png';
    await page.locator('.frame').screenshot({path:path.join(output,file),animations:'disabled'});
    manifest.push({id:state.id,title:state.title,file,ansiSha256:createHash('sha256').update(state.ansi).digest('hex')});
  }
  if(consoleErrors.length)throw new Error('Preview errors: '+consoleErrors.join('\n'));
  await writeFile(path.join(output,'manifest.json'),JSON.stringify({source:'Compiled vesta-setup production view constructors; deterministic preview data',states:manifest},null,2)+'\n');
  const figures=await Promise.all(manifest.map(async state=>{
    const png=(await readFile(path.join(output,state.file))).toString('base64');
    return `<figure id="${state.id}"><figcaption><strong>${state.title.replaceAll('&','&amp;').replaceAll('<','&lt;')}</strong><span>${state.id}</span></figcaption><img alt="${state.id} terminal state" src="data:image/png;base64,${png}"></figure>`;
  }));
  const gallery=`<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Vesta setup — visual states</title><style>body{margin:0;font:14px system-ui;color:#dae2ed;background:#10151d}main{max-width:1000px;margin:auto;padding:24px}h1{font-size:24px}p{color:#a6b3c5;line-height:1.6}nav{display:flex;gap:8px;flex-wrap:wrap;margin:20px 0}nav a{color:#a6ded5;border:1px solid #2e414d;border-radius:6px;padding:5px 8px;text-decoration:none;font-size:12px}figure{margin:0 0 26px}figcaption{display:flex;justify-content:space-between;gap:12px;margin:0 0 8px;color:#dfe9f4}figcaption span{color:#9caebf;font:12px monospace}img{display:block;width:100%;height:auto;border:1px solid #273c48;border-radius:10px}html{scroll-behavior:smooth}</style><main><h1>Vesta host setup</h1><p>${manifest.length} visual states rendered by the compiled terminal installer. Includes keyboard selection, validation errors, startup choices, installation progress, completion, cancellation, resume, narrow layout and plain output. Preview values are synthetic; screenshots do not contain credentials.</p><nav>${manifest.map(s=>`<a href="#${s.id}">${s.id}</a>`).join('')}</nav>${figures.join('')}</main></html>`;
  await writeFile(path.join(output,'gallery.html'),gallery);
  console.log(`Captured ${manifest.length} terminal states in ${output}`);
}finally{await browser.close();}
