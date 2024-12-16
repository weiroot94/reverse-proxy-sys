class ByteUtils {
  static int bytesToInt(List<int> bytes) {
    return bytes.fold(0, (acc, byte) => (acc << 8) + byte);
  }

  static List<int> intToBytes(int value) {
    return [
      (value >> 24) & 0xFF,
      (value >> 16) & 0xFF,
      (value >> 8) & 0xFF,
      value & 0xFF,
    ];
  }
}
