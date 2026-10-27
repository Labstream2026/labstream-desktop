const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync('dist/index.html','utf8');
const rust = fs.readFileSync('src-tauri/src/lib.rs','utf8');
const colorSource = rust.slice(rust.indexOf('  function colorPestana()'), rust.indexOf('  // La FICHA de la página'));
function color({attr=null,meta=null,surfaces=[]}={}) {
 const nodes = surfaces.map(backgroundColor=>({backgroundColor}));
 nodes.forEach((n,i)=>n.parentElement=nodes[i+1]||null);
 const document={documentElement:{getAttribute:()=>attr},body:nodes[0],elementFromPoint:()=>nodes[0],querySelector:()=>meta?{getAttribute:()=>JSON.stringify(meta)}:null};
 return vm.runInNewContext(colorSource+';colorPestana()', {document,innerWidth:1200,getComputedStyle:n=>n});
}
test('live editor surface overrides metadata; metadata overrides the page background',()=>{
 assert.equal(color({attr:'#242424',meta:{color:'#ffffff'},surfaces:['rgb(0, 0, 0)']}),'#242424');
 assert.equal(color({meta:{color:'#262626'},surfaces:['rgb(255, 255, 255)']}),'#262626');
});
test('settings and other pages inherit the visible surface through transparent wrappers',()=>{
 assert.equal(color({surfaces:['rgba(0, 0, 0, 0)','rgb(24, 24, 24)']}),'#181818');
 assert.equal(color({attr:'invalid',meta:{color:'red'},surfaces:['rgb(255, 255, 255)']}),'#ffffff');
 assert.equal(color(),'');
});
const functions=html.slice(html.indexOf('      function esClaro('),html.indexOf('      function render()'));
function bar(tabs,dark=false) {
 const classes=new Set(),styles={};
 const document={documentElement:{classList:{toggle:(n,v)=>v?classes.add(n):classes.delete(n)},style:{setProperty:(n,v)=>styles[n]=v,removeProperty:n=>delete styles[n]}}};
 const ctx={document,estado:{tabs,dark}};vm.runInNewContext(functions+';pintarBarra()',ctx);return {classes,styles};
}
test('active editor colors the entire strip with readable text even when the app theme is light',()=>{
 const b=bar([{active:false,pagina:'#ffffff'},{active:true,pagina:'#242424'}]);
 assert.equal(b.styles['--superficie'],'#242424'); assert.equal(b.styles['--superficie-fg'],'#ececec');assert.ok(b.classes.has('oscuro'));
});
test('switching to a light page restores readable light tabs and supports metadata fallback',()=>{
 const b=bar([{active:false,pagina:'#242424'},{active:true,ficha:{color:'#ffffff'}}],true);
 assert.equal(b.styles['--superficie'],'#ffffff');assert.ok(b.classes.has('claro'));assert.equal(b.styles['--superficie-fg'],'#202124');
 assert.ok(!bar([{active:true}]).classes.has('con-superficie'));
});
test('tab reorder excludes the new-tab button',()=>{
 const src=html.slice(html.indexOf('      function ordenActual()'),html.indexOf('      addEventListener("mousemove"'));
 const result=vm.runInNewContext(src+';ordenActual()',{$tabs:{querySelectorAll:selector=>{assert.equal(selector,'.tab');return [{dataset:{label:'tab-2'}},{dataset:{label:'tab-1'}}];}}});
 assert.deepEqual(Array.from(result),['tab-2','tab-1']);
});
