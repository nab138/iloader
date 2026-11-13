import Foundation
import Security
enum KeychainStore {
 static func save(_ data: Data, service: String, account: String) throws {
  let q: [String: Any] = [kSecClass as String:kSecClassGenericPassword, kSecAttrService as String:service, kSecAttrAccount as String:account]
  SecItemDelete(q as CFDictionary); var item = q; item[kSecValueData as String] = data; item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
  let status = SecItemAdd(item as CFDictionary, nil); guard status == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
 }
 static func read(service: String, account: String) throws -> Data? {
  let q: [String: Any] = [kSecClass as String:kSecClassGenericPassword, kSecAttrService as String:service, kSecAttrAccount as String:account, kSecReturnData as String:true, kSecMatchLimit as String:kSecMatchLimitOne]
  var result: CFTypeRef?; let status = SecItemCopyMatching(q as CFDictionary, &result)
  if status == errSecItemNotFound { return nil }; guard status == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }; return result as? Data
 }
 static func delete(service: String, account: String) throws {
  let q: [String: Any] = [kSecClass as String:kSecClassGenericPassword, kSecAttrService as String:service, kSecAttrAccount as String:account]
  let status = SecItemDelete(q as CFDictionary); guard status == errSecSuccess || status == errSecItemNotFound else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
 }
}