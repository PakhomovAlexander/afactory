import fs from 'node:fs';
export class AuthStorage {
  static create(path) {
    return {
      async read(id) { return JSON.parse(fs.readFileSync(path,'utf8'))[id]; },
      async modify(id,fn) {
        const data=JSON.parse(fs.readFileSync(path,'utf8'));
        const next=await fn(data[id]);if (next === undefined) return data[id];data[id]=next;fs.writeFileSync(path,JSON.stringify(data));return next;
      },
    };
  }
}
