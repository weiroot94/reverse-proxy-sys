/// Utility class for handling byte-int conversions.
class ByteUtils {
  /// Converts a list of bytes into an integer.
  ///
  /// [bytes] is the input list of bytes. The length of the list determines the size.
  /// If [bytes] is empty or too large (>4 bytes), an exception is thrown.
  static int bytesToInt(List<int> bytes, {int size = 4}) {
    if (bytes.isEmpty || bytes.length > size) {
      throw ArgumentError('Invalid byte list length. Expected at most $size bytes.');
    }

    return bytes.fold(0, (acc, byte) => (acc << 8) + (byte & 0xFF));
  }

  /// Converts an integer into a list of bytes.
  ///
  /// [value] is the input integer. [size] specifies the number of bytes in the output (default: 4).
  /// Throws an exception if [size] is not in the range [1, 4].
  static List<int> intToBytes(int value, {int size = 4}) {
    if (size < 1 || size > 4) {
      throw ArgumentError('Invalid size for intToBytes. Must be between 1 and 4.');
    }

    return List.generate(size, (i) => (value >> ((size - 1 - i) * 8)) & 0xFF);
  }
}
