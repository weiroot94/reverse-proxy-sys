import 'dart:async';
import 'utils.dart';

// Command Type constants
const int dataPacket = 0x00;
const int commandPacket = 0x01;

class ProtocolPacket {
  final int sessionId;
  final int packetType;
  final int commandId;
  final List<int> payload;

  ProtocolPacket(this.sessionId, this.packetType, this.commandId, this.payload);
}

class MasterTrafficParser extends StreamTransformerBase<List<int>, ProtocolPacket> {
  final List<int> _buffer = [];
  final int _maxBufferSize = 10 * 1024 * 1024; // 10 MB max buffer size to prevent memory issues

  @override
  Stream<ProtocolPacket> bind(Stream<List<int>> stream) async* {
    await for (var dataChunk in stream) {
      // Add new data to the buffer
      _buffer.addAll(dataChunk);

      // Prevent the buffer from growing uncontrollably
      if (_buffer.length > _maxBufferSize) {
        print('Buffer overflow detected. Clearing buffer to prevent memory issues.');
        _buffer.clear();
        throw Exception('Buffer overflow: Too much unprocessed data in MasterTrafficParser.');
      }

      // Parse packets as long as there is enough data
      while (_buffer.length >= 10) {
        try {
          final packetType = _buffer[0];
          final sessionId = ByteUtils.bytesToInt(_buffer.sublist(1, 5));
          final commandId = _buffer[5];
          final payloadLength = ByteUtils.bytesToInt(_buffer.sublist(6, 10));

          // Ensure the buffer has the full payload for the packet
          if (_buffer.length < 10 + payloadLength) break;

          // Extract and yield the packet
          final payload = _buffer.sublist(10, 10 + payloadLength);
          _buffer.removeRange(0, 10 + payloadLength);

          final packet = ProtocolPacket(sessionId, packetType, commandId, payload);
          yield packet;
        } catch (e, stackTrace) {
          print('Error parsing packet: $e\n$stackTrace');
          _buffer.clear(); // Clear the buffer on critical parsing errors
          throw Exception('Failed to parse ProtocolPacket.');
        }
      }
    }
  }
}
