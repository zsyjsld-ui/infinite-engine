// Original synthetic geometry; generated credentials never enter Git.
import fs from 'node:fs';import path from 'node:path';import crypto from 'node:crypto';
const root=path.resolve(import.meta.dirname,'..'),parent=path.join(root,'.local');fs.mkdirSync(parent,{recursive:true});
const relative=`.local/demo-${Date.now()}-${crypto.randomBytes(4).toString('hex')}`,dir=path.join(root,relative);fs.mkdirSync(dir);
const w=32,h=32,map=Buffer.alloc(32+w*h/8);map.write('IMAP');map.writeUInt16LE(1,4);map.writeUInt16LE(3,6);
for(const [at,value]of [[8,w],[12,h],[16,768],[20,192]])map.writeUInt32LE(value,at);
for(let y=2;y<30;y++){if(y>=10&&y<=13)continue;const i=y*w+12;map[32+(i>>3)]|=1<<(i&7);}
fs.writeFileSync(path.join(dir,'map.bin'),map,{flag:'wx'});
fs.writeFileSync(path.join(dir,'accounts.txt'),`1 ${crypto.randomBytes(32).toString('hex')} 3072 2304 3200\n`,{flag:'wx',mode:0o600});
console.log(`Created synthetic assets in ${relative}. Run from the repository root:\ncargo run -p infinite-server -- --map ${relative}/map.bin --accounts ${relative}/accounts.txt --origin http://127.0.0.1:8080 --port 0 --instance 1 --ready ${relative}/ready.json --stop ${relative}/stop --metrics ${relative}/metrics.json\nWrite ${relative}/stop to shut down. ready.json contains the assigned loopback port. This is a protocol service, not a web page.`);
