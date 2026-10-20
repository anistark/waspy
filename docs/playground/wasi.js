// A small WASI preview 1 host for compiled waspy modules, in the browser and
// in Node alike: the calls a waspy program can import (fd_write, fd_read,
// args_sizes_get, args_get, proc_exit, and for open() path_open,
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
} = {}) {
    let memory = null;
    const input = typeof stdin === 'string' ? new TextEncoder().encode(stdin) : stdin;
    let inputPos = 0;
    const encodedArgs = args.map((a) => new TextEncoder().encode(a));
    // One decoder per stream, so a character split across two writes decodes.
    const decoders = { 1: new TextDecoder('utf-8'), 2: new TextDecoder('utf-8') };
    const sinks = { 1: onStdout, 2: onStderr };
    // fd - FIRST_FILE_FD -> { name, pos, readable, writable }, or null once closed.
    const handles = [];
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
        proc_exit(code) {
            throw new WasiExit(code);
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
            const create = (oflags & 1) !== 0;
            const truncate = (oflags & 8) !== 0;
            const append = (fdflags & 1) !== 0;
            if (truncate) files.set(name, new Uint8Array());
            else if (create && !files.has(name)) files.set(name, new Uint8Array());
            else if (!files.has(name)) return ENOENT;
            const writable = create || truncate || append;
            handles.push({ name, pos: 0, readable: !writable, writable });
            view().setUint32(fdOut, FIRST_FILE_FD + handles.length - 1, true);
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
