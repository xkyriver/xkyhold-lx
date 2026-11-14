// Cross round trip between the TypeScript engine of XkyHold and the Rust
// engine of XkyHold-lx. Copied into a throwaway worktree of the XkyHold
// repository by scripts/cross-ts.sh; never committed there.

import { readFileSync, writeFileSync } from 'node:fs'
import { expect, it } from 'vitest'
import { MemoryDestination } from '../storage/memory'
import { backup } from './backup'
import { fromBase64, fromUtf8, inflate, sha256Hex, toBase64, utf8 } from './bytes'
import * as cryptoModule from './crypto'
import { open as openSealed, unlockRepository } from './crypto'
import { Repository } from './repo'
import { readFile } from './restore'
import { FakeDir, FakeFile, noise } from './testing'

const dir = process.env.CROSS_DIR as string

it('writes a format 1 repository for the Rust engine', async () => {
  const dest = new MemoryDestination()
  const { repo } = await Repository.create(dest, 'pw', {
    argon2: { memoryKiB: 1024, iterations: 1, parallelism: 1 },
    packSize: 4096,
  })
  const tree = new FakeDir('Documents', [
    new FakeFile('a.txt', utf8('hello from TypeScript'), 1_794_400_000_123),
    new FakeFile('big.bin', noise(10_000, 3)),
    new FakeFile('empty.txt', new Uint8Array(0)),
    new FakeDir('Sub', [new FakeFile('é.txt', noise(50, 1))]),
    new FakeDir('Empty'),
  ])
  await backup(repo, [{ name: 'Documents', handle: tree }], { device: 'ts', chunkSize: 4096 })
  const files: Record<string, { sha256: string; mtime: number }> = {}
  const add = async (prefix: string, d: FakeDir) => {
    for (const c of d.children) {
      if (c instanceof FakeDir) await add(`${prefix}${c.name}/`, c)
      else files[`${prefix}${c.name}`] = { sha256: await sha256Hex(c.data), mtime: c.mtime }
    }
  }
  await add('Documents/', tree)
  const objects = Object.fromEntries([...dest.objects].map(([k, v]) => [k, toBase64(v)]))
  writeFileSync(`${dir}/ts-repo.json`, JSON.stringify({ password: 'pw', objects, files }))
})

it('reads the bytes of a format 2 repository written by the Rust engine', async () => {
  const fx = JSON.parse(readFileSync(`${dir}/rust-repo.json`, 'utf8'))
  const dest = new MemoryDestination()
  for (const [k, v] of Object.entries(fx.objects)) dest.objects.set(k, fromBase64(v as string))
  const config = await Repository.readConfig(dest)
  expect(config.format).toBe(2)
  let repo: Repository
  let snap: any
  if ((cryptoModule as { READABLE_FORMATS?: readonly number[] }).READABLE_FORMATS?.includes(2)) {
    // XkyHold with format 2 support (restore-only): the real reader.
    repo = await Repository.open(dest, { password: fx.password })
    expect((repo as unknown as { format: number }).format).toBe(2)
    ;[snap] = await repo.listSnapshots()
  } else {
    // XkyHold 1.0 refuses format 2, as the spec wants (FORMAT-v2.md 3.1).
    await expect(unlockRepository(config, { password: fx.password })).rejects.toThrow(/format 2/)
    // Test harness only: open the keys as format 1 to check every byte below.
    const keys = await unlockRepository({ ...config, format: 1 }, { password: fx.password })
    repo = new Repository(dest, keys)
    await repo.loadIndex()
    // A version 1 reader fails closed on version 2 snapshots (3.2).
    await expect(repo.listSnapshots()).rejects.toThrow()
    const [name] = (await dest.list('snapshots')).map((o) => o.name)
    const sealed = await dest.get('snapshots', name)
    snap = JSON.parse(fromUtf8(await inflate(await openSealed(keys.encKey, sealed, `xkyhold/v2/snapshot/${name}`))))
  }
  expect(snap.format).toBe(2)
  expect(snap.home.uid).toBeTypeOf('number')

  const files: Record<string, string> = {}
  const links: Record<string, string> = {}
  const walk = async (prefix: string, tree: string) => {
    for (const e of (await repo.loadTree(tree)) as any[]) {
      const path = `${prefix}${e.name}`
      if (e.type === 'dir') await walk(`${path}/`, e.subtree)
      else if (e.type === 'symlink') links[path] = e.target
      else {
        const parts: Uint8Array[] = []
        for await (const chunk of readFile(repo, e)) parts.push(chunk)
        const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0))
        let at = 0
        for (const p of parts) {
          all.set(p, at)
          at += p.length
        }
        expect(all.length).toBe(e.size)
        files[path] = await sha256Hex(all)
      }
    }
  }
  await walk('', snap.roots[0].tree)
  expect(files).toEqual(fx.files)
  expect(links).toEqual(fx.links)
})
