import Foundation
final class FileImportService {
 static let shared = FileImportService()
 private init() {}
 func copyIntoDocuments(_ source: URL) throws -> URL {
  let access = source.startAccessingSecurityScopedResource(); defer { if access { source.stopAccessingSecurityScopedResource() } }
  let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("ImportedFiles", isDirectory: true)
  try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
  let target = folder.appendingPathComponent(source.lastPathComponent)
  if FileManager.default.fileExists(atPath: target.path) { try FileManager.default.removeItem(at: target) }
  try FileManager.default.copyItem(at: source, to: target)
  return target
 }
}