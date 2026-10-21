// Minimal on-demand OTLP/HTTP (JSON only) receiver. Appends each request body
// to $AGENT_FABRIC_STATE_ROOT/otel/<signal>-YYYYMMDD.ndjson and exits when idle.
// launchd socket activation: `otlp-collector` (Sockets key "Listeners").
// Foreground: `otlp-collector --listen 127.0.0.1:PORT [--idle-seconds N]`.
import Foundation

@_silgen_name("launch_activate_socket")
func launchActivateSocket(_ name: UnsafePointer<CChar>, _ fds: UnsafeMutablePointer<UnsafeMutablePointer<Int32>?>, _ cnt: UnsafeMutablePointer<Int>) -> Int32

let env = ProcessInfo.processInfo.environment
let stateRoot = env["AGENT_FABRIC_STATE_ROOT"] ?? NSHomeDirectory() + "/.local/state/agent-harness/fabric"
let outDir = stateRoot + "/otel"
let maxBody = Int(env["PROVENANT_OTEL_MAX_BODY_BYTES"] ?? "") ?? 4 << 20
let retentionDays = Int(env["PROVENANT_OTEL_RETENTION_DAYS"] ?? "") ?? 30
let signals: Set<String> = ["logs", "metrics", "traces"]
let lock = NSLock()
nonisolated(unsafe) var active = 0
nonisolated(unsafe) var lastActivity = Date()
nonisolated(unsafe) var prunedDay = ""

func fail(_ msg: String) -> Never { FileHandle.standardError.write(Data((msg + "\n").utf8)); exit(2) }

func stamp(_ date: Date, _ fmt: String) -> String {
    var t = time_t(date.timeIntervalSince1970), tmv = tm()
    gmtime_r(&t, &tmv)
    var buf = [CChar](repeating: 0, count: 40)
    strftime(&buf, buf.count, fmt, &tmv)
    return String(cString: buf)
}

func prune(day: String) {
    guard day != prunedDay else { return }
    prunedDay = day
    let cutoff = Date().addingTimeInterval(-Double(retentionDays) * 86400)
    for name in (try? FileManager.default.contentsOfDirectory(atPath: outDir)) ?? [] where name.hasSuffix(".ndjson") {
        let path = outDir + "/" + name
        if let m = (try? FileManager.default.attributesOfItem(atPath: path))?[.modificationDate] as? Date, m < cutoff {
            try? FileManager.default.removeItem(atPath: path)
        }
    }
}

/// Append one NDJSON line; raw newlines in the (already validated) body are only
/// inter-token whitespace because JSON strings cannot contain them, so blanking is safe.
func store(signal: String, body: Data) -> Bool {
    let now = Date(), day = stamp(now, "%Y%m%d")
    var line = Data("{\"received_at\":\"\(stamp(now, "%Y-%m-%dT%H:%M:%SZ"))\",\"signal\":\"\(signal)\",\"body\":".utf8)
    line.append(contentsOf: body.map { $0 == 10 || $0 == 13 ? 32 : $0 })
    line.append(contentsOf: Array("}\n".utf8))
    lock.lock(); defer { lock.unlock() }
    try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    prune(day: day)
    let fd = open("\(outDir)/\(signal)-\(day).ndjson", O_WRONLY | O_APPEND | O_CREAT, 0o600)
    guard fd >= 0 else { return false }
    defer { close(fd) }
    return line.withUnsafeBytes { write(fd, $0.baseAddress, line.count) } == line.count
}

func reply(_ fd: Int32, _ code: Int, _ text: String, _ body: String, close: Bool) {
    let head = "HTTP/1.1 \(code) \(text)\r\nContent-Type: application/json\r\nContent-Length: \(body.utf8.count)\r\n" + (close ? "Connection: close\r\n" : "") + "\r\n"
    let out = Array((head + body).utf8)
    var sent = 0
    while sent < out.count {
        let n = out[sent...].withUnsafeBytes { send(fd, $0.baseAddress, $0.count, 0) }
        if n <= 0 { return }
        sent += n
    }
}

func errBody(_ m: String) -> String { "{\"error\":\"\(m)\"}" }

func serve(_ fd: Int32) {
    defer { close(fd); lock.lock(); active -= 1; lastActivity = Date(); lock.unlock() }
    var tv = timeval(tv_sec: 10, tv_usec: 0)
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
    var buf = Data()
    var chunk = [UInt8](repeating: 0, count: 65536)
    func fill() -> Bool {
        let n = recv(fd, &chunk, chunk.count, 0)
        if n <= 0 { return false }
        buf.append(contentsOf: chunk[0..<n]); return true
    }
    while true {
        let sep = Data("\r\n\r\n".utf8)
        var range = buf.range(of: sep)
        while range == nil {
            if buf.count > 16384 || !fill() { if buf.count > 16384 { reply(fd, 431, "Header Too Large", errBody("headers too large"), close: true) }; return }
            range = buf.range(of: sep)
        }
        let lines = (String(decoding: buf[..<range!.lowerBound], as: UTF8.self)).components(separatedBy: "\r\n")
        buf.removeSubrange(..<range!.upperBound)
        let parts = lines[0].split(separator: " ")
        var headers: [String: String] = [:]
        for l in lines.dropFirst() { if let i = l.firstIndex(of: ":") { headers[l[..<i].lowercased()] = l[l.index(after: i)...].trimmingCharacters(in: .whitespaces) } }
        guard parts.count == 3 else { reply(fd, 400, "Bad Request", errBody("bad request line"), close: true); return }
        let keep = headers["connection"]?.lowercased() != "close"
        let length = Int(headers["content-length"] ?? "0") ?? -1
        if headers["transfer-encoding"] != nil { reply(fd, 411, "Length Required", errBody("chunked bodies are not supported; send Content-Length"), close: true); return }
        if length < 0 || length > maxBody { reply(fd, 413, "Payload Too Large", errBody("body exceeds \(maxBody) bytes"), close: true); return }
        if headers["expect"]?.lowercased() == "100-continue" { _ = send(fd, "HTTP/1.1 100 Continue\r\n\r\n", 25, 0) }
        while buf.count < length { if !fill() { return } }
        let body = Data(buf.prefix(length)); buf.removeFirst(length)
        lock.lock(); lastActivity = Date(); lock.unlock()

        let path = String(parts[1].split(separator: "?").first ?? "")
        let signal = path.hasPrefix("/v1/") ? String(path.dropFirst(4)) : ""
        let type = (headers["content-type"] ?? "").lowercased()
        if parts[0] != "POST" || !signals.contains(signal) {
            reply(fd, parts[0] == "POST" ? 404 : 405, parts[0] == "POST" ? "Not Found" : "Method Not Allowed", errBody("POST /v1/logs, /v1/metrics or /v1/traces only"), close: !keep)
        } else if !type.hasPrefix("application/json") || headers["content-encoding"] != nil {
            reply(fd, 415, "Unsupported Media Type", errBody("JSON only: set OTEL_EXPORTER_OTLP_PROTOCOL=http/json (protobuf and compressed bodies are not accepted)"), close: !keep)
        } else if (try? JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed])) == nil {
            reply(fd, 400, "Bad Request", errBody("body is not valid JSON"), close: !keep)
        } else if !store(signal: signal, body: body) {
            reply(fd, 500, "Internal Server Error", errBody("storage write failed"), close: true); return
        } else {
            reply(fd, 200, "OK", "{}", close: !keep)
        }
        if !keep { return }
    }
}

// MARK: listener setup
var args = Array(CommandLine.arguments.dropFirst())
func option(_ name: String) -> String? { args.firstIndex(of: name).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
var idleSeconds = Double(option("--idle-seconds") ?? "") ?? (Double(env["PROVENANT_OTEL_IDLE_MINUTES"] ?? "") ?? 10) * 60
var listener: Int32 = -1

if let spec = option("--listen") {
    let bits = spec.split(separator: ":")
    guard bits.count == 2, bits[0] == "127.0.0.1", let port = UInt16(bits[1]) else { fail("--listen requires 127.0.0.1:PORT (loopback only)") }
    listener = socket(AF_INET, SOCK_STREAM, 0)
    var yes: Int32 = 1
    setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &yes, 4)
    var addr = sockaddr_in(sin_len: UInt8(MemoryLayout<sockaddr_in>.size), sin_family: sa_family_t(AF_INET), sin_port: port.bigEndian, sin_addr: in_addr(s_addr: UInt32(0x7f000001).bigEndian), sin_zero: (0, 0, 0, 0, 0, 0, 0, 0))
    let ok = withUnsafePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
    guard ok == 0, listen(listener, 16) == 0 else { fail("cannot listen on \(spec): \(String(cString: strerror(errno)))") }
} else {
    var fds: UnsafeMutablePointer<Int32>? = nil
    var count = 0
    guard launchActivateSocket("Listeners", &fds, &count) == 0, count > 0, let fds else { fail("no launchd socket 'Listeners'; use --listen 127.0.0.1:PORT") }
    listener = fds[0]
    free(fds)
}
signal(SIGPIPE, SIG_IGN)
if let l = option("--listen") { print("listening \(l)"); fflush(stdout) }

while true {
    var p = pollfd(fd: listener, events: Int16(POLLIN), revents: 0)
    if poll(&p, 1, 500) > 0 {
        let c = accept(listener, nil, nil)
        if c >= 0 {
            lock.lock(); active += 1; lastActivity = Date(); lock.unlock()
            Thread.detachNewThread { serve(c) }
        }
    }
    lock.lock(); let idle = active == 0 && Date().timeIntervalSince(lastActivity) > idleSeconds; lock.unlock()
    if idle { exit(0) }
}
