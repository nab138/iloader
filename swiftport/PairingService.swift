import Foundation
final class PairingService {
 static let shared = PairingService()
 private init() {}
 private var directory: URL { FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("PairingFiles", isDirectory: true) }
 func importFile(from source: URL) throws -> PairingFile {
  let access = source.startAccessingSecurityScopedResource(); defer { if access { source.stopAccessingSecurityScopedResource() } }
  try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
  let target = directory.appendingPathComponent(source.lastPathComponent)
  if FileManager.default.fileExists(atPath: target.path) { try FileManager.default.removeItem(at: target) }
  try FileManager.default.copyItem(at: source, to: target)
  let size = (try? target.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
  return PairingFile(name: target.deletingPathExtension().lastPathComponent, filename: target.lastPathComponent, importedAt: Date(), byteCount: size)
 }
 func listFiles() -> [PairingFile] {
  guard let urls = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey]) else { return [] }
  return urls.filter { !$0.hasDirectoryPath }.map { url in
   let values = try? url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
   return PairingFile(name: url.deletingPathExtension().lastPathComponent, filename: url.lastPathComponent, importedAt: values?.contentModificationDate ?? .distantPast, byteCount: values?.fileSize ?? 0)
  }.sorted { $0.importedAt > $1.importedAt }
 }
 func url(for file: PairingFile) -> URL { directory.appendingPathComponent(file.filename) }
 func delete(_ file: PairingFile) throws { try FileManager.default.removeItem(at: url(for: file)) }
}