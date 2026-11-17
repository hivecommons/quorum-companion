// quorum-capture: the macOS capture module of the Quorum companion.
//
// The Tauri app runs this helper as a child process. It never talks to the
// network; it only reads audio on this Mac and writes to stdout.
//
//   quorum-capture detect          JSON lines on stdout when a meeting app starts or stops using
//                                  the microphone (Core Audio process objects, macOS 14.2+).
//   quorum-capture list            One JSON array of the meeting apps that are running.
//   quorum-capture capture <bundle-id>
//                                  Captures that app's audio (ScreenCaptureKit, no virtual driver)
//                                  and the microphone (AVAudioEngine) as 16 kHz mono s16le PCM.
//                                  Frames on stdout: 1 byte channel (0 = call, 1 = mic),
//                                  4 bytes little-endian length, then the PCM bytes.
//
// The helper exits when its stdin closes, so it never outlives the app. Logs go to stderr.

import AVFoundation
import CoreAudio
import Foundation
import ScreenCaptureKit

// MARK: - Meeting apps

/// Meeting apps the companion offers to listen to: a process bundle id prefix (helper processes
/// such as "com.google.Chrome.helper" match their app), the app bundle id to capture, and the
/// name hive records. Order matters: the first matching prefix wins.
let meetingApps: [(prefix: String, bundle: String, app: String)] = [
    ("us.zoom.", "us.zoom.xos", "zoom"),
    ("com.microsoft.teams2", "com.microsoft.teams2", "teams"),
    ("com.microsoft.teams", "com.microsoft.teams", "teams"),
    ("com.tinyspeck.slackmacgap", "com.tinyspeck.slackmacgap", "slack"),
    ("Cisco-Systems.Spark", "Cisco-Systems.Spark", "webex"),
    ("com.cisco.webexmeetingsapp", "com.cisco.webexmeetingsapp", "webex"),
    ("com.google.Chrome", "com.google.Chrome", "browser"),
    ("com.microsoft.edgemac", "com.microsoft.edgemac", "browser"),
    ("company.thebrowser.Browser", "company.thebrowser.Browser", "browser"),
    ("com.apple.Safari", "com.apple.Safari", "browser"),
    ("com.apple.WebKit", "com.apple.Safari", "browser"),
]

/// The app to capture for a process bundle id, or nil when it is not a meeting app.
func appBundle(for bundle: String) -> (bundle: String, app: String)? {
    for entry in meetingApps where bundle.hasPrefix(entry.prefix) {
        return (entry.bundle, entry.app)
    }
    return nil
}

func log(_ message: String) {
    FileHandle.standardError.write(Data(("quorum-capture: " + message + "\n").utf8))
}

let outputQueue = DispatchQueue(label: "quorum-capture.stdout")

func emitJSON(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
    outputQueue.sync {
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
}

/// Exits when the parent closes stdin (or dies), so the helper never keeps capturing alone.
func exitWhenStdinCloses() {
    Thread.detachNewThread {
        while true {
            let data = FileHandle.standardInput.availableData
            if data.isEmpty { exit(0) }
        }
    }
}

// MARK: - Core Audio process objects (detection)

func propertyAddress(_ selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
}

func processObjects() -> [AudioObjectID] {
    var address = propertyAddress(kAudioHardwarePropertyProcessObjectList)
    var size: UInt32 = 0
    let system = AudioObjectID(kAudioObjectSystemObject)
    guard AudioObjectGetPropertyDataSize(system, &address, 0, nil, &size) == noErr, size > 0 else { return [] }
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    guard AudioObjectGetPropertyData(system, &address, 0, nil, &size, &ids) == noErr else { return [] }
    return ids
}

func bundleID(of process: AudioObjectID) -> String? {
    var address = propertyAddress(kAudioProcessPropertyBundleID)
    var value: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    guard AudioObjectGetPropertyData(process, &address, 0, nil, &size, &value) == noErr, let v = value else { return nil }
    return v.takeRetainedValue() as String
}

func isRunningInput(_ process: AudioObjectID) -> Bool {
    var address = propertyAddress(kAudioProcessPropertyIsRunningInput)
    var value: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    guard AudioObjectGetPropertyData(process, &address, 0, nil, &size, &value) == noErr else { return false }
    return value != 0
}

/// Meeting apps currently using the microphone, keyed by the app bundle id to capture.
func appsInCall() -> [String: String] {
    var out: [String: String] = [:]
    for process in processObjects() where isRunningInput(process) {
        if let bundle = bundleID(of: process), let app = appBundle(for: bundle) {
            out[app.bundle] = app.app
        }
    }
    return out
}

/// Polls every two seconds; a call ends after two polls without microphone use, so a short
/// mute or device switch does not end it.
func runDetect() -> Never {
    exitWhenStdinCloses()
    let pollSeconds: UInt32 = 2
    let missesToEnd = 2
    var active: [String: (app: String, misses: Int)] = [:]
    while true {
        let now = appsInCall()
        for (bundle, app) in now where active[bundle] == nil {
            emitJSON(["event": "call_started", "bundle_id": bundle, "app": app])
        }
        for (bundle, state) in active {
            if now[bundle] != nil {
                active[bundle] = (state.app, 0)
            } else if state.misses + 1 >= missesToEnd {
                active[bundle] = nil
                emitJSON(["event": "call_ended", "bundle_id": bundle, "app": state.app])
                continue
            } else {
                active[bundle] = (state.app, state.misses + 1)
            }
        }
        for (bundle, app) in now where active[bundle] == nil {
            active[bundle] = (app, 0)
        }
        sleep(pollSeconds)
    }
}

func runList() -> Never {
    let inCall = appsInCall()
    var seen = Set<String>()
    var apps: [[String: Any]] = []
    for running in NSWorkspace.shared.runningApplications {
        guard let bundle = running.bundleIdentifier, let app = appBundle(for: bundle), !seen.contains(app.bundle) else { continue }
        seen.insert(app.bundle)
        apps.append(["bundle_id": app.bundle, "app": app.app, "name": running.localizedName ?? app.app, "in_call": inCall[app.bundle] != nil])
    }
    if let data = try? JSONSerialization.data(withJSONObject: apps, options: [.sortedKeys]) {
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
    exit(0)
}

// MARK: - Capture

let channelCall: UInt8 = 0
let channelMic: UInt8 = 1

func writeFrame(channel: UInt8, _ pcm: Data) {
    guard !pcm.isEmpty else { return }
    var header = Data([channel])
    var length = UInt32(pcm.count).littleEndian
    header.append(Data(bytes: &length, count: MemoryLayout<UInt32>.size))
    outputQueue.sync {
        FileHandle.standardOutput.write(header)
        FileHandle.standardOutput.write(pcm)
    }
}

/// Converts any PCM buffer to 16 kHz mono s16le, the format hive and Quorum take.
final class Resampler {
    let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: true)!
    private var converter: AVAudioConverter?
    private var source: AVAudioFormat?

    func convert(_ input: AVAudioPCMBuffer) -> Data? {
        if converter == nil || source != input.format {
            converter = AVAudioConverter(from: input.format, to: target)
            source = input.format
        }
        guard let converter, input.frameLength > 0 else { return nil }
        let ratio = target.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return nil }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if fed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            inputStatus.pointee = .haveData
            return input
        }
        guard status != .error, out.frameLength > 0, let samples = out.int16ChannelData else { return nil }
        return Data(bytes: samples[0], count: Int(out.frameLength) * MemoryLayout<Int16>.size)
    }
}

func pcmBuffer(from sample: CMSampleBuffer) -> AVAudioPCMBuffer? {
    guard let description = CMSampleBufferGetFormatDescription(sample),
          let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description),
          let format = AVAudioFormat(streamDescription: asbd) else { return nil }
    let frames = AVAudioFrameCount(CMSampleBufferGetNumSamples(sample))
    guard frames > 0, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames) else { return nil }
    buffer.frameLength = frames
    let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(sample, at: 0, frameCount: Int32(frames), into: buffer.mutableAudioBufferList)
    return status == noErr ? buffer : nil
}

final class CallAudio: NSObject, SCStreamOutput, SCStreamDelegate {
    let resampler = Resampler()

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid, let buffer = pcmBuffer(from: sampleBuffer),
              let pcm = resampler.convert(buffer) else { return }
        writeFrame(channel: channelCall, pcm)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        log("call audio stopped: \(error.localizedDescription)")
        exit(3)
    }
}

let callAudio = CallAudio()
let micEngine = AVAudioEngine()
let micResampler = Resampler()
var callStream: SCStream?

func startMicrophone() {
    let input = micEngine.inputNode
    let format = input.outputFormat(forBus: 0)
    let tapFrames: AVAudioFrameCount = 4096
    input.installTap(onBus: 0, bufferSize: tapFrames, format: format) { buffer, _ in
        if let pcm = micResampler.convert(buffer) {
            writeFrame(channel: channelMic, pcm)
        }
    }
    do {
        try micEngine.start()
    } catch {
        log("microphone unavailable: \(error.localizedDescription)")
    }
}

func startCallAudio(bundle: String) async throws {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
    guard let app = content.applications.first(where: { $0.bundleIdentifier == bundle }) else {
        throw NSError(domain: "quorum-capture", code: 2, userInfo: [NSLocalizedDescriptionKey: "\(bundle) is not running"])
    }
    guard let display = content.displays.first else {
        throw NSError(domain: "quorum-capture", code: 2, userInfo: [NSLocalizedDescriptionKey: "no display to anchor the capture filter"])
    }
    let filter = SCContentFilter(display: display, including: [app], exceptingWindows: [])
    let config = SCStreamConfiguration()
    config.capturesAudio = true
    config.excludesCurrentProcessAudio = true
    config.sampleRate = 48000
    config.channelCount = 1
    // Audio only: the smallest video the stream allows, at one frame per second, never read.
    config.width = 2
    config.height = 2
    config.minimumFrameInterval = CMTime(value: 1, timescale: 1)
    let stream = SCStream(filter: filter, configuration: config, delegate: callAudio)
    try stream.addStreamOutput(callAudio, type: .audio, sampleHandlerQueue: DispatchQueue(label: "quorum-capture.call"))
    try await stream.startCapture()
    callStream = stream
}

func runCapture(bundle: String) -> Never {
    exitWhenStdinCloses()
    AVCaptureDevice.requestAccess(for: .audio) { granted in
        if granted {
            startMicrophone()
        } else {
            log("microphone access denied: capturing the call audio only")
        }
    }
    Task {
        do {
            try await startCallAudio(bundle: bundle)
            emitJSONToStderr(["event": "capturing", "bundle_id": bundle])
        } catch {
            log("cannot capture \(bundle): \(error.localizedDescription) (System Settings, Privacy & Security, Screen & System Audio Recording)")
            exit(2)
        }
    }
    dispatchMain()
}

func emitJSONToStderr(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
    FileHandle.standardError.write(data)
    FileHandle.standardError.write(Data("\n".utf8))
}

// MARK: - Main

let args = CommandLine.arguments
switch args.count > 1 ? args[1] : "" {
case "detect":
    runDetect()
case "list":
    runList()
case "capture" where args.count > 2:
    runCapture(bundle: args[2])
default:
    log("usage: quorum-capture detect | list | capture <bundle-id>")
    exit(64)
}
