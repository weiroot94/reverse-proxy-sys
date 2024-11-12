import 'dart:io';
import 'package:http/http.dart' as http;
import 'package:synchronized/synchronized.dart';
import 'dart:async';
import 'dart:typed_data';
import 'dart:convert';

import 'socks5_session.dart';
import 'version.dart';

enum ProxyState { disconnected, connecting, connected, }

// Command Type constants
const int dataPacket = 0x00;
const int commandPacket = 0x01;
// Command ID constants
const int speedCheck = 0x01;
const int versionCheck = 0x02;
const int heartbeatCheck = 0x03;

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
        final sessionId = _bytesToInt(_buffer.sublist(1, 5));
        final commandId = _buffer[5];
        final payloadLength = _bytesToInt(_buffer.sublist(6, 10));

        if (_buffer.length < 10 + payloadLength) break;

        final payload = _buffer.sublist(10, 10 + payloadLength);
        _buffer.removeRange(0, 10 + payloadLength);

        yield ProtocolPacket(sessionId, packetType, commandId, payload);
      }
    }
  }

  int _bytesToInt(List<int> bytes) {
    return bytes.fold(0, (acc, byte) => (acc << 8) + byte);
  }
}

class TunSocks {
  final String host;
  final int port;

  late Socket? _masterConn;
  final _masterConnLock = Lock();
  ProxyState currentState = ProxyState.disconnected;
  int retryAttempts = 0;
  List<int> accumulatedBuffer = [];
  bool isManuallyStopped = false;
  
  // Heartbeat Monitoring
  Timer? _heartbeatTimer;
  final Duration heartbeatInterval = Duration(seconds: 15);

  TunSocks({
    required this.host,
    required this.port,
  });

  Future<void> startTunnel() async {
    if (currentState != ProxyState.disconnected) {
        return;
    }

    currentState = ProxyState.connecting;
    isManuallyStopped = false;

    try {
      _masterConn = await Socket.connect(host, port).timeout(Duration(seconds: 5));

      print('Connected to master node at $host:$port');
      currentState = ProxyState.connected;

      _masterConn!
          .transform(StreamTransformer.fromBind((stream) => MasterTrafficParser().bind(stream)))
          .listen((packet) async {
            if (packet.packetType == dataPacket) {
              // Handle data packets through SOCKS5 sessions
              final session = sessions.putIfAbsent(
                  packet.sessionId,
                  () => Socks5Session(packet.sessionId, _masterConn, _sendDataPacket));
              session.processSocks5Data(packet.payload);

            } else if (packet.packetType == commandPacket) {
              await _handleCommand(packet.commandId, packet.payload);
            }
          },
          onDone: () {
            print('Connection to master was closed.');
            _handleDisconnection();
            },
          onError: (error) {
            print('Master connection error: $error');
            _handleDisconnection();
          });
      
      _manageHeartbeatTimer();
    } catch (e) {
      print('Failed to connect to master: $e');
      _handleDisconnection();
    }
  }

  Future<void> _handleCommand(int? commandId, List<int> payload) async {
    switch (commandId) {
      case speedCheck:
        await _performSpeedTest(payload);
        break;
      case versionCheck:
        _sendVersionInfo();
        break;
      case heartbeatCheck:
        _sendAliveResponse();
        _manageHeartbeatTimer();
        break;
      default:
        print('Unknown command received');
    }
  }

  void _handleDisconnection() {
    currentState = ProxyState.disconnected;
    _cleanupConnections();

    if (!isManuallyStopped) {
      _retryConnection();
    }
  }

  Future<void> _retryConnection() async {
    final retryDelay = (retryAttempts % 5 == 0 && retryAttempts != 0)
        ? Duration(minutes: 1)
        : Duration(seconds: 5);
    retryAttempts++;
    await Future.delayed(retryDelay);

    print('Retrying connection... Attempt $retryAttempts');
    startTunnel();
  }

  void _manageHeartbeatTimer() {
    // Cancel any existing heartbeat timer
    _heartbeatTimer?.cancel();

    // Start a new timer to wait for the next heartbeat
    _heartbeatTimer = Timer(heartbeatInterval, () {
      print("No heartbeat from master, reconnecting...");
      _handleDisconnection();
    });
  }

  Future<void> _performSpeedTest(List<int> payload) async {
    String url = utf8.decode(payload);
    final stopwatch = Stopwatch()..start();

    try {
      final response = await http.get(Uri.parse(url));
      stopwatch.stop();

      if (response.statusCode == 200) {
        final contentLength = response.contentLength ?? 0;
        final elapsedTime = stopwatch.elapsedMilliseconds / 1000; // in seconds

        // Calculate speed in Mbps
        final speedMbps = (contentLength * 8) / (elapsedTime * 1000000);

        _sendSpeedResult(speedMbps);
      } else {
        print('Failed to test URL: ${response.statusCode}');
      }
    } catch (e) {
      print('Error during speed test: $e');
    }
  }

  int _bytesToInt(List<int> bytes) {
    return bytes.fold(0, (previousValue, element) => (previousValue << 8) + element);
  }

  List<int> _intToBytes(int value) {
    return [
      (value >> 24) & 0xFF,
      (value >> 16) & 0xFF,
      (value >> 8) & 0xFF,
      value & 0xFF,
    ];
  }

  void _sendToMasterInProtocol(int sessionId, List<int> payload, {int packetType = dataPacket, int commandId = 0x00}) async {
    await _masterConnLock.synchronized(() async {
      if (_masterConn != null) {
        final packet = [
          packetType,                      // Packet Type (0x00 for data, 0x01 for command)
          ..._intToBytes(sessionId),       // Session ID
          commandId,                       // Command ID (default to 0x00 for data packets)
          ..._intToBytes(payload.length),  // Payload Length
          ...payload                       // Payload data
        ];
        try {
          _masterConn?.add(packet);
          await _masterConn?.flush();
        } catch (e) {
          print('Failed to send to master: $e');
        }
      }
    });
  }

  // Command packet example:
  void _sendSpeedResult(double speedMbps) {
    final payload = utf8.encode('$speedMbps');
    _sendToMasterInProtocol(0, payload, packetType: commandPacket, commandId: speedCheck);
  }

  void _sendVersionInfo() {
    final payload = utf8.encode(slaveVersion);
    _sendToMasterInProtocol(0, payload, packetType: commandPacket, commandId: versionCheck);
  }

  void _sendAliveResponse() {
    _sendToMasterInProtocol(0, utf8.encode("ALIVE"), packetType: commandPacket, commandId: heartbeatCheck);
  }

  // Data packet example:
  void _sendDataPacket(int sessionId, List<int> data) {
    _sendToMasterInProtocol(sessionId, data);
  }

  void stopTunnel() {
    isManuallyStopped = true;
    _cleanupConnections();
    print('Proxy server stopped.');
  }

  void _cleanupConnections() {
    _masterConnLock.synchronized(() async {
      _masterConn?.close();
      _masterConn = null;
    });
    currentState = ProxyState.disconnected;
  }
}
