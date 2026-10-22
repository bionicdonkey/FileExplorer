import AppKit
import ExplorerAdapters
import ExplorerDomain

private final class FileCollectionView: NSCollectionView {
    var menuProvider: ((IndexPath?) -> NSMenu?)?
    var onReturn: (() -> Void)?

    override func menu(for event: NSEvent) -> NSMenu? {
        menuProvider?(indexPathForItem(at: convert(event.locationInWindow, from: nil)))
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 { onReturn?() } else { super.keyDown(with: event) }
    }

    override var acceptsFirstResponder: Bool { true }
}

private final class GroupHeader: NSView, NSCollectionViewElement {
    static let identifier = NSUserInterfaceItemIdentifier("group-header")
    let label = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12), label.centerYAnchor.constraint(equalTo: centerYAnchor)])
    }

    required init?(coder: NSCoder) { fatalError() }
}

/// One tile: the icon (a thumbnail for images and video) with the name, below it or beside it.
private final class FileTile: NSCollectionViewItem {
    static let identifier = NSUserInterfaceItemIdentifier("file-tile")
    let icon = NSImageView()
    let name = NSTextField(wrappingLabelWithString: "")
    let detail = NSTextField(labelWithString: "")
    var thumbnailRequest: ThumbnailService.Request?
    var token = UUID()
    private var constraintsInstalled: [NSLayoutConstraint] = []
    private var background = NSView()

    override func loadView() {
        view = NSView()
        view.wantsLayer = true
        background.wantsLayer = true
        background.layer?.cornerRadius = 8
        background.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(background)
        for subview in [icon, name, detail] as [NSView] { subview.translatesAutoresizingMaskIntoConstraints = false; view.addSubview(subview) }
        icon.imageScaling = .scaleProportionallyUpOrDown
        name.maximumNumberOfLines = 2
        name.lineBreakMode = .byTruncatingMiddle
        name.cell?.truncatesLastVisibleLine = true
        name.isSelectable = false
        detail.textColor = .secondaryLabelColor
        detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        NSLayoutConstraint.activate([
            background.leadingAnchor.constraint(equalTo: view.leadingAnchor), background.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            background.topAnchor.constraint(equalTo: view.topAnchor), background.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    override var isSelected: Bool { didSet { updateBackground() } }

    private func updateBackground() {
        background.layer?.backgroundColor = isSelected ? NSColor.selectedContentBackgroundColor.withAlphaComponent(0.35).cgColor : NSColor.clear.cgColor
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        thumbnailRequest?.cancel()
        thumbnailRequest = nil
        token = UUID()
        updateBackground()
    }

    func configure(item: FileItem, displayName: String, options: ViewOptions, services: AppServices) {
        NSLayoutConstraint.deactivate(constraintsInstalled)
        let points = CGFloat(options.gridSize.iconSize)
        let hybrid = options.viewMode == .hybrid
        name.stringValue = displayName
        name.alignment = hybrid ? .left : .center
        name.font = .systemFont(ofSize: NSFont.systemFontSize)
        detail.isHidden = !hybrid
        detail.stringValue = hybrid ? (item.isDirectory ? Formatters.type(item) : "\(ByteFormat.string(item.size)) · \(Formatters.date(item.modified))") : ""
        icon.image = IconCache.shared.icon(for: item, points: Int(points))
        view.setAccessibilityLabel(item.name)
        view.setAccessibilityRole(.cell)
        let pad = CGFloat(options.gridSize.layout.verticalPadding) + 4
        if hybrid {
            constraintsInstalled = [
                icon.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 8), icon.centerYAnchor.constraint(equalTo: view.centerYAnchor),
                icon.widthAnchor.constraint(equalToConstant: points), icon.heightAnchor.constraint(equalToConstant: points),
                name.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 8), name.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -8),
                name.bottomAnchor.constraint(equalTo: view.centerYAnchor, constant: 2),
                detail.leadingAnchor.constraint(equalTo: name.leadingAnchor), detail.trailingAnchor.constraint(equalTo: name.trailingAnchor),
                detail.topAnchor.constraint(equalTo: view.centerYAnchor, constant: 2),
            ]
        } else {
            constraintsInstalled = [
                icon.topAnchor.constraint(equalTo: view.topAnchor, constant: pad), icon.centerXAnchor.constraint(equalTo: view.centerXAnchor),
                icon.widthAnchor.constraint(equalToConstant: points), icon.heightAnchor.constraint(equalToConstant: points),
                name.topAnchor.constraint(equalTo: icon.bottomAnchor, constant: 4),
                name.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 4), name.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -4),
            ]
        }
        NSLayoutConstraint.activate(constraintsInstalled)
        updateBackground()
        guard options.thumbnails, item.isVisualMedia else { return }
        let current = token
        let scale = view.window?.backingScaleFactor ?? NSScreen.main?.backingScaleFactor ?? 2
        thumbnailRequest = services.thumbnails.thumbnail(for: item.location, size: CGSize(width: points, height: points), scale: scale) { [weak self] image in
            guard let self, self.token == current, let image else { return }
            self.icon.image = image
        }
    }
}

/// Icon-grid and hybrid views: an NSCollectionView with a flow layout, grouped into sections.
final class FileGridView: NSObject, FileView, NSCollectionViewDataSource, NSCollectionViewDelegateFlowLayout {
    weak var host: FileViewHost?
    private let scroll = NSScrollView()
    private let collection = FileCollectionView()
    private let layout = NSCollectionViewFlowLayout()
    private var sections: [(title: String?, items: [FileItem])] = []
    private var applying = false
    private let services: AppServices

    var view: NSView { scroll }
    var focusView: NSView { collection }

    init(services: AppServices) {
        self.services = services
        super.init()
        collection.collectionViewLayout = layout
        collection.dataSource = self
        collection.delegate = self
        collection.isSelectable = true
        collection.allowsMultipleSelection = true
        collection.backgroundColors = [.controlBackgroundColor]
        collection.register(FileTile.self, forItemWithIdentifier: FileTile.identifier)
        collection.register(GroupHeader.self, forSupplementaryViewOfKind: NSCollectionView.elementKindSectionHeader, withIdentifier: GroupHeader.identifier)
        collection.registerForDraggedTypes([.fileURL])
        collection.setDraggingSourceOperationMask([.copy, .move], forLocal: false)
        collection.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        collection.setAccessibilityLabel("Files")
        collection.menuProvider = { [weak self] path in self?.menu(for: path) }
        collection.onReturn = { [weak self] in self?.selectedItems.first.map { self?.host?.fileViewActivate($0) } }
        scroll.documentView = collection
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(boundsChanged), name: NSView.boundsDidChangeNotification, object: scroll.contentView)
    }

    deinit { NotificationCenter.default.removeObserver(self) }

    // MARK: reload

    func reload() {
        guard let tab = host?.tab else { return }
        applying = true
        defer { applying = false }
        sections = tab.groups
        let options = tab.options
        let tile = options.gridSize.layout
        let hybrid = options.viewMode == .hybrid
        let icon = CGFloat(options.gridSize.iconSize)
        layout.itemSize = hybrid ? NSSize(width: CGFloat(tile.tileWidth) + 60, height: max(icon + 16, 52))
                                 : NSSize(width: max(CGFloat(tile.tileWidth) * 0.7, icon + 24), height: icon + 56 + CGFloat(tile.verticalPadding))
        layout.minimumInteritemSpacing = CGFloat(tile.gap) / 2
        layout.minimumLineSpacing = CGFloat(options.gridSize.rowSpacing) + 4
        layout.sectionInset = NSEdgeInsets(top: 8, left: 12, bottom: 12, right: 12)
        layout.headerReferenceSize = sections.first?.title == nil ? .zero : NSSize(width: 0, height: 28)
        collection.reloadData()
        applySelection()
        DispatchQueue.main.async { [weak self] in self?.reportVisible() }
    }

    private func item(at path: IndexPath) -> FileItem? {
        sections.indices.contains(path.section) && sections[path.section].items.indices.contains(path.item) ? sections[path.section].items[path.item] : nil
    }

    private var selectedItems: [FileItem] { collection.selectionIndexPaths.sorted().compactMap(item(at:)) }

    func applySelection() {
        guard let tab = host?.tab else { return }
        let keys = Set(tab.state.selectionKeys)
        var paths = Set<IndexPath>()
        for (s, section) in sections.enumerated() { for (i, item) in section.items.enumerated() where keys.contains(item.identityKey) { paths.insert(IndexPath(item: i, section: s)) } }
        if paths != collection.selectionIndexPaths {
            applying = true
            collection.selectionIndexPaths = paths
            applying = false
        }
    }

    func scroll(to item: FileItem) {
        for (s, section) in sections.enumerated() {
            if let i = section.items.firstIndex(where: { $0.identityKey == item.identityKey }) {
                collection.scrollToItems(at: [IndexPath(item: i, section: s)], scrollPosition: .nearestHorizontalEdge)
            }
        }
    }

    @objc private func boundsChanged() { reportVisible() }

    private func reportVisible() {
        let items = collection.indexPathsForVisibleItems().sorted().compactMap(item(at:))
        host?.fileViewVisible(items)
    }

    // MARK: data

    func numberOfSections(in collectionView: NSCollectionView) -> Int { sections.count }

    func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int { sections[section].items.count }

    func collectionView(_ collectionView: NSCollectionView, itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
        let tile = collectionView.makeItem(withIdentifier: FileTile.identifier, for: indexPath)
        if let tile = tile as? FileTile, let item = item(at: indexPath), let options = host?.tab.options {
            tile.configure(item: item, displayName: host?.tab.displayName(for: item) ?? item.name, options: options, services: services)
        }
        return tile
    }

    func collectionView(_ collectionView: NSCollectionView, viewForSupplementaryElementOfKind kind: NSCollectionView.SupplementaryElementKind, at indexPath: IndexPath) -> NSView {
        let header = collectionView.makeSupplementaryView(ofKind: kind, withIdentifier: GroupHeader.identifier, for: indexPath)
        (header as? GroupHeader)?.label.stringValue = sections[indexPath.section].title ?? ""
        return header
    }

    func collectionView(_ collectionView: NSCollectionView, layout collectionViewLayout: NSCollectionViewLayout, referenceSizeForHeaderInSection section: Int) -> NSSize {
        sections[section].title == nil ? .zero : NSSize(width: 0, height: 28)
    }

    // MARK: selection and activation

    func collectionView(_ collectionView: NSCollectionView, didSelectItemsAt indexPaths: Set<IndexPath>) { selectionChanged() }
    func collectionView(_ collectionView: NSCollectionView, didDeselectItemsAt indexPaths: Set<IndexPath>) { selectionChanged() }

    private func selectionChanged() {
        guard !applying else { return }
        host?.fileViewSelectionChanged(selectedItems)
    }

    private func menu(for path: IndexPath?) -> NSMenu? {
        if let path, item(at: path) != nil {
            if !collection.selectionIndexPaths.contains(path) { collection.selectionIndexPaths = [path]; selectionChanged() }
        } else if !collection.selectionIndexPaths.isEmpty {
            collection.selectionIndexPaths = []
            selectionChanged()
        }
        return host?.fileViewMenu(for: selectedItems)
    }

    func beginRename(_ item: FileItem) {
        RenamePrompt.ask(for: item, in: collection.window) { [weak self] name in self?.host?.fileViewRename(item, to: name) }
    }

    // MARK: drag and drop

    func collectionView(_ collectionView: NSCollectionView, pasteboardWriterForItemAt indexPath: IndexPath) -> NSPasteboardWriting? { item(at: indexPath)?.pasteboardURL }

    func collectionView(_ collectionView: NSCollectionView, validateDrop draggingInfo: NSDraggingInfo, proposedIndexPath proposedDropIndexPath: AutoreleasingUnsafeMutablePointer<NSIndexPath>,
                        dropOperation proposedDropOperation: UnsafeMutablePointer<NSCollectionView.DropOperation>) -> NSDragOperation {
        guard let host else { return [] }
        if proposedDropOperation.pointee == .on, let target = item(at: proposedDropIndexPath.pointee as IndexPath), target.isDirectory {
            return DropSupport.validate(draggingInfo, host: host, destination: target.location)
        }
        return DropSupport.validate(draggingInfo, host: host, destination: host.tab.location)
    }

    func collectionView(_ collectionView: NSCollectionView, acceptDrop draggingInfo: NSDraggingInfo, indexPath: IndexPath, dropOperation: NSCollectionView.DropOperation) -> Bool {
        guard let host else { return false }
        if dropOperation == .on, let target = item(at: indexPath), target.isDirectory {
            return DropSupport.accept(draggingInfo, host: host, destination: target.location)
        }
        return DropSupport.accept(draggingInfo, host: host, destination: host.tab.location)
    }
}
