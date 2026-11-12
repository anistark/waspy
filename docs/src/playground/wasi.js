// A small WASI preview 1 host for compiled waspy modules, in the browser and
// in Node alike: the calls a waspy program can import (fd_write, fd_read,
// args_sizes_get, args_get, environ_sizes_get, environ_get, proc_exit,
// random_get, and for open(), os.remove, os.rename, os.listdir, and os.path
// path_open, path_filestat_get, path_unlink_file, path_rename, fd_readdir,
// fd_prestat_get, fd_prestat_dir_name, fd_close), standard output and error
// handed to callbacks as text, standard input served from bytes, an exit
// raised as a WasiExit the caller can catch, and files kept in a Map (one
// preopened directory, `.`, at fd 3). Nothing here touches a real filesystem.
//
//   const wasi = createWasi({ args: ['main.py'], stdin: 'bob\n', onStdout, onStderr, files });
//   const { instance } = await WebAssembly.instantiate(bytes, wasi.imports);
//   wasi.bind(instance);

const ESUCCESS = 0;
const EBADF = 8;
const ENOENT = 44;
const ENOTDIR = 54;
const PREOPEN_FD = 3;
const FIRST_FILE_FD = 4;

export class WasiExit extends Error {
    constructor(code) {
        super(`exited with status ${code}`);
        this.code = code;
    }
}

export function createWasi({
    args = ['main.py'],
    stdin = '',
    onStdout = () => {},
    onStderr = () => {},
    files = new Map(),
    env = {},
} = {}) {
    let memory = null;
    const input = typeof stdin === 'string' ? new TextEncoder().encode(stdin) : stdin;
    let inputPos = 0;
    const encodedArgs = args.map((a) => new TextEncoder().encode(a));
    const encodedEnv = Object.entries(env).map(([k, v]) => new TextEncoder().encode(`${k}=${v}`));
    // One decoder per stream, so a character split across two writes decodes.
    const decoders = { 1: new TextDecoder('utf-8'), 2: new TextDecoder('utf-8') };
    const sinks = { 1: onStdout, 2: onStderr };
    // fd - FIRST_FILE_FD -> { name, pos, readable, writable }, or null once closed.
    const handles = [];
    // Each file's modification time, in nanoseconds: stamped when it is
    // created or written to, and kept by a rename.
    const mtimes = new Map();
    const now = () => BigInt(Date.now()) * 1000000n;
    // The entries of a directory: `.` and `..`, then each name in it, sorted,
    // a directory being a prefix of a file's name (3 a directory, 4 a file).
    const entries = (dir) => {
        const trimmed = dir.replace(/^\.\//, '').replace(/\/+$/, '');
        const prefix = trimmed === '' || trimmed === '.' ? '' : `${trimmed}/`;
        const names = new Map();
        for (const file of files.keys()) {
            const path = file.replace(/^\.\//, '');
            if (!path.startsWith(prefix)) continue;
            const rest = path.slice(prefix.length);
            const slash = rest.indexOf('/');
            if (slash >= 0) names.set(rest.slice(0, slash), 3);
            else names.set(rest, 4);
        }
        return [['.', 3], ['..', 3], ...[...names.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))];
    };
    const handle = (fd) => (fd >= FIRST_FILE_FD ? handles[fd - FIRST_FILE_FD] : null);

    const view = () => new DataView(memory.buffer);
    const bytes = () => new Uint8Array(memory.buffer);

    const wasi = {
        fd_write(fd, iovs, count, done) {
            const file = handle(fd);
            if (file) {
                if (!file.writable) return EBADF;
                const dv = view();
                let total = 0;
                for (let k = 0; k < count; k++) {
                    const base = dv.getUint32(iovs + 8 * k, true);
                    const len = dv.getUint32(iovs + 8 * k + 4, true);
                    const old = files.get(file.name) ?? new Uint8Array();
                    const grown = new Uint8Array(old.length + len);
                    grown.set(old);
                    grown.set(bytes().subarray(base, base + len), old.length);
                    files.set(file.name, grown);
                    mtimes.set(file.name, now());
                    total += len;
                }
                dv.setUint32(done, total, true);
                return ESUCCESS;
            }
            const sink = sinks[fd];
            if (!sink) return EBADF;
            const dv = view();
            let total = 0;
            for (let k = 0; k < count; k++) {
                const base = dv.getUint32(iovs + 8 * k, true);
                const len = dv.getUint32(iovs + 8 * k + 4, true);
                sink(decoders[fd].decode(bytes().subarray(base, base + len), { stream: true }));
                total += len;
            }
            dv.setUint32(done, total, true);
            return ESUCCESS;
        },
        fd_read(fd, iovs, count, done) {
            const file = handle(fd);
            if (file) {
                if (!file.readable) return EBADF;
                const contents = files.get(file.name) ?? new Uint8Array();
                const dv = view();
                let total = 0;
                for (let k = 0; k < count; k++) {
                    const base = dv.getUint32(iovs + 8 * k, true);
                    const len = dv.getUint32(iovs + 8 * k + 4, true);
                    const take = Math.min(len, contents.length - file.pos);
                    bytes().set(contents.subarray(file.pos, file.pos + take), base);
                    file.pos += take;
                    total += take;
                }
                dv.setUint32(done, total, true);
                return ESUCCESS;
            }
            if (fd !== 0) return EBADF;
            const dv = view();
            let total = 0;
            for (let k = 0; k < count; k++) {
                const base = dv.getUint32(iovs + 8 * k, true);
                const len = dv.getUint32(iovs + 8 * k + 4, true);
                const take = Math.min(len, input.length - inputPos);
                bytes().set(input.subarray(inputPos, inputPos + take), base);
                inputPos += take;
                total += take;
            }
            dv.setUint32(done, total, true);
            return ESUCCESS;
        },
        args_sizes_get(argc, size) {
            const dv = view();
            dv.setUint32(argc, encodedArgs.length, true);
            dv.setUint32(size, encodedArgs.reduce((n, a) => n + a.length + 1, 0), true);
            return ESUCCESS;
        },
        args_get(argv, buf) {
            const dv = view();
            let at = buf;
            encodedArgs.forEach((a, k) => {
                dv.setUint32(argv + 4 * k, at, true);
                bytes().set(a, at);
                bytes()[at + a.length] = 0;
                at += a.length + 1;
            });
            return ESUCCESS;
        },
        environ_sizes_get(count, size) {
            const dv = view();
            dv.setUint32(count, encodedEnv.length, true);
            dv.setUint32(size, encodedEnv.reduce((n, e) => n + e.length + 1, 0), true);
            return ESUCCESS;
        },
        environ_get(ptrs, buf) {
            const dv = view();
            let at = buf;
            encodedEnv.forEach((e, k) => {
                dv.setUint32(ptrs + 4 * k, at, true);
                bytes().set(e, at);
                bytes()[at + e.length] = 0;
                at += e.length + 1;
            });
            return ESUCCESS;
        },
        proc_exit(code) {
            throw new WasiExit(code);
        },
        clock_time_get(clock, precision, out) {
            // Nanoseconds since the epoch, for datetime.now().
            view().setBigUint64(out, BigInt(Date.now()) * 1000000n, true);
            return ESUCCESS;
        },
        random_get(buf, len) {
            // getRandomValues fills at most 65536 bytes per call.
            for (let at = 0; at < len; at += 65536) {
                globalThis.crypto.getRandomValues(bytes().subarray(buf + at, buf + Math.min(len, at + 65536)));
            }
            return ESUCCESS;
        },
        fd_prestat_get(fd, buf) {
            if (fd !== PREOPEN_FD) return EBADF;
            const dv = view();
            dv.setUint8(buf, 0); // a directory
            dv.setUint32(buf + 4, 1, true);
            return ESUCCESS;
        },
        fd_prestat_dir_name(fd, path) {
            if (fd !== PREOPEN_FD) return EBADF;
            bytes()[path] = 0x2e; // '.'
            return ESUCCESS;
        },
        path_open(dir, _lookup, path, len, oflags, _rights, _inheriting, fdflags, fdOut) {
            if (dir !== PREOPEN_FD) return EBADF;
            const name = new TextDecoder().decode(bytes().subarray(path, path + len));
            // A directory, for fd_readdir: neither readable nor writable.
            if ((oflags & 2) !== 0) {
                if (files.has(name)) return ENOTDIR;
                if (entries(name).length === 2 && name !== '.' && name !== '') return ENOENT;
                handles.push({ name, pos: 0, readable: false, writable: false, dir: true });
                view().setUint32(fdOut, FIRST_FILE_FD + handles.length - 1, true);
                return ESUCCESS;
            }
            const create = (oflags & 1) !== 0;
            const truncate = (oflags & 8) !== 0;
            const append = (fdflags & 1) !== 0;
            if (truncate) {
                files.set(name, new Uint8Array());
                mtimes.set(name, now());
            } else if (create && !files.has(name)) {
                files.set(name, new Uint8Array());
                mtimes.set(name, now());
            } else if (!files.has(name)) return ENOENT;
            const writable = create || truncate || append;
            handles.push({ name, pos: 0, readable: !writable, writable });
            view().setUint32(fdOut, FIRST_FILE_FD + handles.length - 1, true);
            return ESUCCESS;
        },
        path_filestat_get(dir, _flags, path, len, out) {
            if (dir !== PREOPEN_FD) return EBADF;
            const name = new TextDecoder().decode(bytes().subarray(path, path + len)).replace(/^\.\//, '');
            // The preopen is a directory, a file a regular file, and a prefix
            // of a file's name a directory holding it.
            const dirName = name.replace(/\/+$/, '') + '/';
            let filetype;
            if (name === '.' || name === '') filetype = 3;
            else if (files.has(name)) filetype = 4;
            else if ([...files.keys()].some((f) => f.startsWith(dirName))) filetype = 3;
            else return ENOENT;
            bytes().fill(0, out, out + 64);
            bytes()[out + 16] = filetype;
            view().setBigUint64(out + 32, BigInt(files.get(name)?.length ?? 0), true);
            view().setBigUint64(out + 48, mtimes.get(name) ?? 0n, true);
            return ESUCCESS;
        },
        path_unlink_file(dir, path, len) {
            if (dir !== PREOPEN_FD) return EBADF;
            const name = new TextDecoder().decode(bytes().subarray(path, path + len)).replace(/^\.\//, '');
            mtimes.delete(name);
            return files.delete(name) ? ESUCCESS : ENOENT;
        },
        path_rename(dir, oldPath, oldLen, newDir, newPath, newLen) {
            if (dir !== PREOPEN_FD || newDir !== PREOPEN_FD) return EBADF;
            const text = (at, n) => new TextDecoder().decode(bytes().subarray(at, at + n)).replace(/^\.\//, '');
            const from = text(oldPath, oldLen);
            if (!files.has(from)) return ENOENT;
            const to = text(newPath, newLen);
            files.set(to, files.get(from));
            files.delete(from);
            if (mtimes.has(from)) {
                mtimes.set(to, mtimes.get(from));
                mtimes.delete(from);
            }
            return ESUCCESS;
        },
        fd_readdir(fd, buf, bufLen, cookie, used) {
            const dir = handle(fd);
            if (!dir || !dir.dir) return EBADF;
            const parts = [];
            entries(dir.name).forEach(([name, kind], i) => {
                if (BigInt(i) < BigInt(cookie)) return;
                const encoded = new TextEncoder().encode(name);
                const entry = new Uint8Array(24 + encoded.length);
                const dv = new DataView(entry.buffer);
                dv.setBigUint64(0, BigInt(i + 1), true);
                dv.setUint32(16, encoded.length, true);
                entry[20] = kind;
                entry.set(encoded, 24);
                parts.push(entry);
            });
            // As much as fits: the last entry may be cut short.
            let at = 0;
            for (const entry of parts) {
                if (at >= bufLen) break;
                const take = Math.min(entry.length, bufLen - at);
                bytes().set(entry.subarray(0, take), buf + at);
                at += take;
            }
            view().setUint32(used, at, true);
            return ESUCCESS;
        },
        fd_close(fd) {
            if (!handle(fd)) return EBADF;
            handles[fd - FIRST_FILE_FD] = null;
            return ESUCCESS;
        },
    };

    return {
        imports: { wasi_snapshot_preview1: wasi },
        bind(instance) {
            memory = instance.exports.memory;
        },
    };
}
