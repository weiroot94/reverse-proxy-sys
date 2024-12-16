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

  @override
  Stream<ProtocolPacket> bind(Stream<List<int>> stream) async* {
    await for (var dataChunk in stream) {
      _buffer.addAll(dataChunk);

      while (_buffer.length >= 10) {
        final packetType = _buffer[0];
        final sessionId = ByteUtils.bytesToInt(_buffer.sublist(1, 5));
        final commandId = _buffer[5];
        final payloadLength = ByteUtils.bytesToInt(_buffer.sublist(6, 10));

        if (_buffer.length < 10 + payloadLength) break;

        final payload = _buffer.sublist(10, 10 + payloadLength);
        _buffer.removeRange(0, 10 + payloadLength);

        yield ProtocolPacket(sessionId, packetType, commandId, payload);
      }
    }
  }
}