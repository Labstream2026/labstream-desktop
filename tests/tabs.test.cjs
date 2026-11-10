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
// ── Ventanas nuevas (1.37): el window.open parcheado y la ventana «en blanco» de ventanaDiferida ──
// Se ejecuta el trozo del script de la pestaña en node:vm. URL hay que pasarlo a mano: un contexto de vm no lo trae.
const shellJs=rust.slice(rust.indexOf('  function abs(h)'),rust.indexOf('  // ── Errores de la página'))+rust.slice(rust.indexOf('  function ventanaDiferida('),rust.indexOf('  // Atajos de teclado.'));
function shell(){
 const llamadas=[],nativas=[];
 const window={open:(...a)=>{nativas.push(a);return 'nativa';}};
 const T={core:{invoke:(o,a)=>llamadas.push([o,a])},event:{emit:(n,p)=>llamadas.push([n,p])}};
 vm.runInNewContext(shellJs,{window,T,ORIGIN:'https://os.labstreamsas.com',location:{href:'https://os.labstreamsas.com/cotizaciones/propuestas/1'},URL});
 return {open:(...a)=>window.open(...a),llamadas,nativas};
}
// Lo creado dentro del contexto de vm tiene otros prototipos: se compara en plano.
const plano=x=>JSON.parse(JSON.stringify(x));
test('about:blank lleva al navegador cuando la página le pone una dirección de fuera',()=>{
 const s=shell();const w=s.open('about:blank','_blank');
 assert.ok(w);assert.equal(s.nativas.length,0);assert.equal(s.llamadas.length,0);
 w.opener=null;w.location.href='https://wa.me/573001234567?text=hola';
 assert.deepEqual(plano(s.llamadas),[['plugin:opener|open_url',{url:'https://wa.me/573001234567?text=hola',with:null}]]);
 assert.equal(w.closed,false);w.close();assert.equal(w.closed,true);
});
test('la ventana en blanco con dirección interna abre pestaña y conserva el nombre ls-',()=>{
 let s=shell();s.open('','ls-edicion-9').location.assign('/proyectos/1');
 assert.deepEqual(plano(s.llamadas),[['ls-new-tab',{url:'https://os.labstreamsas.com/proyectos/1',name:'ls-edicion-9'}]]);
 s=shell();const w=s.open();w.location='https://os.labstreamsas.com/x';
 assert.deepEqual(plano(s.llamadas),[['ls-new-tab',{url:'https://os.labstreamsas.com/x'}]]);
 assert.equal(s.nativas.length,0);
});
test('solo cuenta la primera dirección http(s)',()=>{
 const s=shell();const w=s.open('about:blank');
 w.location.href='javascript:alert(1)';assert.equal(s.llamadas.length,0);
 w.location.href='https://wa.me/1';assert.equal(s.llamadas.length,1);
 w.location.href='https://wa.me/2';assert.equal(s.llamadas.length,1);
 assert.equal(plano(s.llamadas)[0][1].url,'https://wa.me/1');
 // cerrada antes de tener dirección, ya no abre nada
 const c=shell();const v=c.open('about:blank');v.close();v.location.href='https://wa.me/3';assert.equal(c.llamadas.length,0);
});
test('window.open mantiene sus caminos de siempre',()=>{
 const s=shell();
 assert.equal(s.open('https://wa.me/x'),null);assert.deepEqual(plano(s.llamadas[0]),['plugin:opener|open_url',{url:'https://wa.me/x',with:null}]);
 assert.equal(s.open('/docs/file/1','_blank'),null);assert.deepEqual(plano(s.llamadas[1]),['ls-new-tab',{url:'https://os.labstreamsas.com/docs/file/1'}]);
 assert.equal(s.open('blob:https://os.labstreamsas.com/u'),'nativa');assert.equal(s.nativas.length,1);assert.equal(s.llamadas.length,2);
});
