import Cocoa
import ScreenCaptureKit
import Dispatch

// MARK: - Custom Header View for Window Dragging
class HeaderDragView: NSView {
    private var initialLocation: NSPoint = .zero

    override func mouseDown(with event: NSEvent) {
        initialLocation = event.locationInWindow
    }

    override func mouseDragged(with event: NSEvent) {
        guard let window = self.window else { return }
        let currentLocation = event.locationInWindow
        let deltaX = currentLocation.x - initialLocation.x
        let deltaY = currentLocation.y - initialLocation.y
        let currentOrigin = window.frame.origin
        window.setFrameOrigin(NSPoint(x: currentOrigin.x + deltaX, y: currentOrigin.y + deltaY))
    }
}

// MARK: - Non-Activating Floating Window
class NonActivatingWindow: NSWindow {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

// MARK: - PiP Controller
class PipController: NSObject {
    static let shared = PipController()

    let windowWidth: CGFloat = 360
    let windowHeight: CGFloat = 236
    let margin: CGFloat = 16

    var window: NonActivatingWindow!
    var visualEffectView: NSVisualEffectView!
    var appBadge: NSTextField!
    var imageView: NSImageView!
    var placeholderLabel: NSTextField!
    var dotView: NSView!

    var targetPid: pid_t = 0
    var targetAppName: String = ""
    var lastActionAt = Date()
    var isDismissed = false
    var isCapturing = false
    var captureTask: Task<Void, Never>?

    override init() {
        super.init()
        setupUI()
    }

    func setupUI() {
        let (x, y) = getTopRightPosition()
        let rect = NSRect(x: x, y: y, width: windowWidth, height: windowHeight)

        window = NonActivatingWindow(
            contentRect: rect,
            styleMask: [.borderless, .resizable],
            backing: .buffered,
            defer: false
        )
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        window.level = .floating
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.minSize = NSSize(width: 220, height: 150)

        // Root container with rounded corners and border
        let contentView = NSView(frame: rect)
        contentView.wantsLayer = true
        contentView.layer?.cornerRadius = 10
        contentView.layer?.masksToBounds = true
        contentView.layer?.borderWidth = 1.0
        contentView.layer?.borderColor = NSColor(white: 0.3, alpha: 0.5).cgColor

        // Background visual effect (frosted glass)
        visualEffectView = NSVisualEffectView(frame: contentView.bounds)
        visualEffectView.autoresizingMask = [.width, .height]
        visualEffectView.material = .hudWindow
        visualEffectView.blendingMode = .behindWindow
        visualEffectView.state = .active
        contentView.addSubview(visualEffectView)

        // Header view
        let headerHeight: CGFloat = 28
        let headerView = HeaderDragView(frame: NSRect(x: 0, y: windowHeight - headerHeight, width: windowWidth, height: headerHeight))
        headerView.autoresizingMask = [.width, .minYMargin]
        headerView.wantsLayer = true
        headerView.layer?.borderWidth = 0.5
        headerView.layer?.borderColor = NSColor(white: 0.2, alpha: 0.3).cgColor

        // Green dot
        dotView = NSView(frame: NSRect(x: 10, y: (headerHeight - 8) / 2, width: 8, height: 8))
        dotView.wantsLayer = true
        dotView.layer?.cornerRadius = 4
        dotView.layer?.backgroundColor = NSColor(calibratedRed: 0.18, green: 0.80, blue: 0.44, alpha: 1.0).cgColor
        let pulse = CABasicAnimation(keyPath: "opacity")
        pulse.fromValue = 1.0
        pulse.toValue = 0.4
        pulse.duration = 1.4
        pulse.autoreverses = true
        pulse.repeatCount = .infinity
        dotView.layer?.add(pulse, forKey: "pulse")
        headerView.addSubview(dotView)

        // Brand title
        let brandLabel = NSTextField(labelWithString: "Codex Computer Use")
        brandLabel.font = NSFont.systemFont(ofSize: 11, weight: .bold)
        brandLabel.textColor = NSColor.labelColor
        brandLabel.frame = NSRect(x: 24, y: 5, width: 130, height: 18)
        headerView.addSubview(brandLabel)

        // App badge
        appBadge = NSTextField(labelWithString: "")
        appBadge.font = NSFont.systemFont(ofSize: 10, weight: .medium)
        appBadge.textColor = NSColor.controlAccentColor
        appBadge.alignment = .center
        appBadge.wantsLayer = true
        appBadge.layer?.cornerRadius = 8
        appBadge.layer?.masksToBounds = true
        appBadge.layer?.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.15).cgColor
        appBadge.layer?.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.4).cgColor
        appBadge.layer?.borderWidth = 0.5
        appBadge.frame = NSRect(x: 156, y: 5, width: 90, height: 18)
        appBadge.isHidden = true
        headerView.addSubview(appBadge)

        // Close button (rightmost)
        let closeBtn = NSButton(frame: NSRect(x: windowWidth - 26, y: 4, width: 20, height: 20))
        closeBtn.autoresizingMask = [.minXMargin]
        closeBtn.bezelStyle = .regularSquare
        closeBtn.isBordered = false
        closeBtn.title = "✕"
        closeBtn.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        closeBtn.toolTip = "关闭（本次会话不再弹出）"
        closeBtn.target = self
        closeBtn.action = #selector(onCloseClicked)
        headerView.addSubview(closeBtn)

        // Jump button
        let jumpBtn = NSButton(frame: NSRect(x: windowWidth - 48, y: 4, width: 20, height: 20))
        jumpBtn.autoresizingMask = [.minXMargin]
        jumpBtn.bezelStyle = .regularSquare
        jumpBtn.isBordered = false
        jumpBtn.title = "↗"
        jumpBtn.font = NSFont.systemFont(ofSize: 12, weight: .bold)
        jumpBtn.toolTip = "切到该应用"
        jumpBtn.target = self
        jumpBtn.action = #selector(onJumpClicked)
        headerView.addSubview(jumpBtn)

        contentView.addSubview(headerView)

        // Body: Image view
        let bodyRect = NSRect(x: 0, y: 0, width: windowWidth, height: windowHeight - headerHeight)
        imageView = NSImageView(frame: bodyRect)
        imageView.autoresizingMask = [.width, .height]
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.imageAlignment = .alignCenter
        contentView.addSubview(imageView)

        // Placeholder
        placeholderLabel = NSTextField(labelWithString: "等待画面…")
        placeholderLabel.font = NSFont.systemFont(ofSize: 12)
        placeholderLabel.textColor = NSColor.secondaryLabelColor
        placeholderLabel.alignment = .center
        placeholderLabel.frame = NSRect(x: 0, y: (bodyRect.height - 20) / 2, width: windowWidth, height: 20)
        placeholderLabel.autoresizingMask = [.width, .minYMargin, .maxYMargin]
        contentView.addSubview(placeholderLabel)

        window.contentView = contentView
    }

    func getTopRightPosition() -> (CGFloat, CGFloat) {
        guard let screen = NSScreen.main else {
            return (100, 100)
        }
        let visible = screen.visibleFrame
        let x = visible.maxX - windowWidth - margin
        let y = visible.maxY - windowHeight - 10
        return (x, y)
    }

    @objc func onCloseClicked() {
        isDismissed = true
        hide()
    }

    @objc func onJumpClicked() {
        guard targetPid > 0 else { return }
        if let app = NSRunningApplication(processIdentifier: targetPid) {
            app.activate(options: .activateAllWindows)
            hide()
        }
    }

    func show() {
        guard !isDismissed else { return }
        DispatchQueue.main.async {
            self.window.orderFrontRegardless()
        }
    }

    func hide() {
        DispatchQueue.main.async {
            self.window.orderOut(nil)
        }
    }

    func updateTarget(pid: pid_t, appName: String, bundleId: String) {
        DispatchQueue.main.async {
            var resolvedPid = pid
            if resolvedPid == 0 && (!appName.isEmpty || !bundleId.isEmpty) {
                if let app = NSWorkspace.shared.runningApplications.first(where: {
                    (!bundleId.isEmpty && $0.bundleIdentifier == bundleId) ||
                    (!appName.isEmpty && $0.localizedName?.lowercased() == appName.lowercased())
                }) {
                    resolvedPid = app.processIdentifier
                }
            }

            if self.targetPid != resolvedPid || self.targetAppName != appName {
                self.isDismissed = false
            }
            self.targetPid = resolvedPid
            self.targetAppName = appName
            self.lastActionAt = Date()

            let displayName = !appName.isEmpty ? appName : bundleId
            if !displayName.isEmpty {
                self.appBadge.stringValue = displayName
                self.appBadge.isHidden = false
            } else {
                self.appBadge.isHidden = true
            }

            self.checkVisibility()
            self.startCaptureIfNeeded()
        }
    }

    func updateFrame(data: Data) {
        DispatchQueue.main.async {
            if let image = NSImage(data: data) {
                self.imageView.image = image
                self.placeholderLabel.isHidden = true
            }
            self.lastActionAt = Date()
            self.checkVisibility()
        }
    }

    func checkVisibility() {
        guard targetPid > 0 else {
            hide()
            return
        }
        if Date().timeIntervalSince(lastActionAt) > 20.0 {
            hide()
            return
        }
        if let frontmost = NSWorkspace.shared.frontmostApplication,
           frontmost.processIdentifier == targetPid {
            hide()
            return
        }
        show()
    }

    func startCaptureIfNeeded() {
        guard !isCapturing, targetPid > 0 else { return }
        isCapturing = true

        captureTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 120_000_000) // ~8 fps
                guard let self = self, self.targetPid > 0, !self.isDismissed else { break }

                // Check if target is frontmost or idle
                await MainActor.run {
                    self.checkVisibility()
                }
                let isVisible = await MainActor.run { self.window.isVisible }
                guard isVisible else { continue }

                do {
                    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                    let targetWindow = content.windows.first {
                        $0.owningApplication?.processID == self.targetPid &&
                        $0.frame.width >= 120 && $0.frame.height >= 80
                    }
                    guard let window = targetWindow else { continue }

                    let filter = SCContentFilter(desktopIndependentWindow: window)
                    let config = SCStreamConfiguration()
                    config.width = max(1, Int(window.frame.width / 2))
                    config.height = max(1, Int(window.frame.height / 2))
                    config.showsCursor = true

                    let cgImage = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
                    await MainActor.run {
                        let nsImage = NSImage(cgImage: cgImage, size: NSZeroSize)
                        self.imageView.image = nsImage
                        self.placeholderLabel.isHidden = true
                    }
                } catch {
                    // Ignore capture errors (window minimized or permissions)
                }
            }
            self?.isCapturing = false
        }
    }
}

// MARK: - Stdio Message Processing Loop
func startStdioLoop() {
    DispatchQueue.global(qos: .userInitiated).async {
        while let line = readLine() {
            guard let data = line.data(using: .utf8),
                  let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                continue
            }
            let event = json["event"] as? String ?? ""
            switch event {
            case "target":
                let pid = (json["pid"] as? Int).map { pid_t($0) } ?? 0
                let appName = json["appName"] as? String ?? ""
                let bundleId = json["bundleId"] as? String ?? ""
                PipController.shared.updateTarget(pid: pid, appName: appName, bundleId: bundleId)
            case "frame":
                if let base64Str = json["data"] as? String {
                    let cleanBase64 = base64Str.contains(",") ? String(base64Str.split(separator: ",")[1]) : base64Str
                    if let frameData = Data(base64Encoded: cleanBase64) {
                        PipController.shared.updateFrame(data: frameData)
                    }
                }
            case "hide":
                PipController.shared.hide()
            case "quit":
                exit(0)
            default:
                break
            }
        }
        // stdin EOF -> parent process exited, terminate cleanly
        exit(0)
    }
}

// MARK: - Main Application Entry
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
_ = PipController.shared

// Timer to poll activity & idle timeout
Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
    PipController.shared.checkVisibility()
}

// Command line test argument
if CommandLine.arguments.contains("--test") {
    print("Running in test mode for 3 seconds...")
    if let sshyard = NSWorkspace.shared.runningApplications.first(where: { $0.bundleIdentifier == "com.sshyard.desktop" }) {
        PipController.shared.updateTarget(pid: sshyard.processIdentifier, appName: "SSHYard", bundleId: "com.sshyard.desktop")
    } else {
        PipController.shared.updateTarget(pid: getpid(), appName: "TestApp", bundleId: "test.app")
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
        exit(0)
    }
} else {
    startStdioLoop()
}

app.run()
