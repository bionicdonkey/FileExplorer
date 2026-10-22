import AppKit
import ExplorerDomain

/// The strip under the files: item and selection counts on the left, the running operations on the
/// right (a click opens the operations page).
final class StatusBar: NSView {
    private let label = NSTextField(labelWithString: "")
    private let operationLabel = NSTextField(labelWithString: "")
    private let progress = NSProgressIndicator()
    private let operationsButton = NSButton()
    var onShowOperations: (NSView) -> Void = { _ in }

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        label.lineBreakMode = .byTruncatingTail
        label.setAccessibilityIdentifier("status-text")
        operationLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        operationLabel.textColor = .secondaryLabelColor
        operationLabel.lineBreakMode = .byTruncatingMiddle
        progress.style = .bar
        progress.controlSize = .small
        progress.isHidden = true
        operationsButton.image = Symbol.image("list.bullet.rectangle", description: "Operations")
        operationsButton.isBordered = false
        operationsButton.target = self
        operationsButton.action = #selector(showOperations)
        operationsButton.toolTip = "Show operations"
        operationsButton.setAccessibilityLabel("Operations")
        operationsButton.setAccessibilityIdentifier("operations-button")
        let separator = NSBox()
        separator.boxType = .separator
        for subview in [separator, label, operationLabel, progress, operationsButton] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; addSubview(subview) }
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 26),
            separator.topAnchor.constraint(equalTo: topAnchor), separator.leadingAnchor.constraint(equalTo: leadingAnchor), separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12), label.centerYAnchor.constraint(equalTo: centerYAnchor),
            operationsButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10), operationsButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            progress.trailingAnchor.constraint(equalTo: operationsButton.leadingAnchor, constant: -8), progress.centerYAnchor.constraint(equalTo: centerYAnchor),
            progress.widthAnchor.constraint(equalToConstant: 90),
            operationLabel.trailingAnchor.constraint(equalTo: progress.leadingAnchor, constant: -8), operationLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            operationLabel.leadingAnchor.constraint(greaterThanOrEqualTo: label.trailingAnchor, constant: 12),
            operationLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 320),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func show(text: String) { label.stringValue = text; label.setAccessibilityLabel(text) }

    func show(operations manager: OperationManager) {
        let active = manager.activeRecords
        guard let first = active.first else {
            operationLabel.stringValue = ""
            progress.isHidden = true
            progress.stopAnimation(nil)
            return
        }
        let extra = active.count > 1 ? " (+\(active.count - 1) more)" : ""
        operationLabel.stringValue = OperationsViewController.title(for: first) + extra
        progress.isHidden = false
        progress.isIndeterminate = first.total == 0
        progress.minValue = 0
        progress.maxValue = Double(max(first.total, 1))
        progress.doubleValue = Double(first.completed)
        if progress.isIndeterminate { progress.startAnimation(nil) }
        progress.setAccessibilityLabel("Operation progress")
    }

    @objc private func showOperations() { onShowOperations(operationsButton) }
}
