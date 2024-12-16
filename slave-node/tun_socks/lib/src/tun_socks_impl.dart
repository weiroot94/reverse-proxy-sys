import 'dart:io';
import 'package:http/http.dart' as http;
import 'package:synchronized/synchronized.dart';
import 'dart:async';
import 'dart:typed_data';
import 'dart:convert';

import 'socks5_session.dart';
import 'protocol.dart';
import 'utils.dart';
import 'version.dart';

enum ProxyState { disconnected, connecting, connected, }

class TunSocks {
  final String host;
  final int port;

  Socket? _masterConn;
  final _masterConnLock = Lock();
  ProxyState currentState = ProxyState.disconnected;
  int retryAttempts = 0;
  List<int> accumulatedBuffer = [];
  bool isManuallyStopped = false;
  
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
              await _handleData(packet.sessionId, packet.payload);
            } else if (packet.packetType == commandPacket) {
              await _handleCommand(packet.sessionId, packet.commandId, packet.payload);
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
    } catch (e) {
      print('Failed to connect to master: $e');
      _handleDisconnection();
    }
  }

  Future<void> _handleData(int sessionId, List<int> payload) async {
    final session = sessions[sessionId];
    if (session != null) {
      await session.processSocks5Data(payload);
    } else {
      print('No session found for session ID $sessionId. Dropping data packet.');
    }
  }

  Future<void> _handleCommand(int sessionId, int? commandId, List<int> payload) async {
    switch (commandId) {
      case speedCheck:
        await _performSpeedTest(payload);
        break;
      case versionCheck:
        _sendVersionInfo();
        break;
      case heartbeatCheck:
        print('heartbeat command received');
        _sendAliveResponse();
        break;
      case urlCheck:
        await _performUrlCheck(payload);
        break;
      case initSession:
        _handleInitSession(sessionId, payload);
        break;
      default:
        print('Unknown command received: Command ID $commandId with session ID $sessionId');
    }
  }

  Future<void> _handleInitSession(int sessionId, List<int> payload) async {
    if (sessions.containsKey(sessionId)) {
      print('Session $sessionId already exists.');
      return;
    }

    final destinationInfo = utf8.decode(payload);
    final splitIndex = destinationInfo.lastIndexOf(':');
    if (splitIndex == -1) {
      print('Invalid destination info: $destinationInfo');
      return;
    }

    final address = destinationInfo.substring(0, splitIndex);
    final port = int.tryParse(destinationInfo.substring(splitIndex + 1));

    if (port == null) {
      print('Invalid port in destination info: $destinationInfo');
      return;
    }

    final session = Socks5Session(sessionId, _sendDataPacket);
    sessions[sessionId] = session;
    await session.initialize(address, port);
  }

  void _handleDisconnection() {
    if (currentState == ProxyState.disconnected) return;

    _cleanupConnections();

    currentState = ProxyState.disconnected;

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

  Future<void> _performUrlCheck(List<int> payload) async {
    final targetUrl = utf8.decode(payload);
    try {
      final response = await http.get(Uri.parse(targetUrl));
      if (response.statusCode == 200) {
        _sendUrlCheckResponse(response.body);
      } else {
        print('Failed to fetch location data: ${response.statusCode}');
      }
    } catch (e) {
      print('Error fetching location data: $e');
    }
  }

  void _sendToMasterInProtocol(int sessionId, List<int> payload, {int packetType = dataPacket, int commandId = 0x00}) async {
    await _masterConnLock.synchronized(() async {
      if (_masterConn == null || currentState != ProxyState.connected) {
        print('Attempted to send data, but connection is not active.');
        return;
      }
      final packet = [
        packetType,                      // Packet Type (0x00 for data, 0x01 for command)
        ...ByteUtils.intToBytes(sessionId),       // Session ID
        commandId,                       // Command ID (default to 0x00 for data packets)
        ...ByteUtils.intToBytes(payload.length),  // Payload Length
        ...payload                       // Payload data
      ];
      try {
        _masterConn?.add(packet);
        await _masterConn?.flush();
      } catch (e) {
        print('Failed to send to master: $e');
        _handleDisconnection();
      }
    });
  }

  // Command packet example:
  void _sendSpeedResult(double speedMbps) {
    final payload = utf8.encode('$speedMbps');
    _sendToMasterInProtocol(0, payload, packetType: commandPacket, commandId: speedCheck);
  }

  void _sendUrlCheckResponse(String response) {
    final payload = utf8.encode(response);
    _sendToMasterInProtocol(0, payload, packetType: commandPacket, commandId: urlCheck);
  }

  void _sendVersionInfo() {
    print('sending version $slaveVersion');
    final payload = utf8.encode(slaveVersion);
    _sendToMasterInProtocol(0, payload, packetType: commandPacket, commandId: versionCheck);
    print('sent versionfino to master');
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
      if (_masterConn != null) {
        try {
          await _masterConn?.close();
        } catch (e) {
          print('Error while closing master connection: $e');
        } finally {
          _masterConn = null;
        }
      }
    });
    currentState = ProxyState.disconnected;
  }
}
