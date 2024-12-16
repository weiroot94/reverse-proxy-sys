import 'dart:io';
import 'package:http/http.dart' as http;
import 'package:synchronized/synchronized.dart';
import 'dart:async';
import 'dart:typed_data';
import 'dart:convert';

import 'command_handler.dart';
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

  final CommandHandler commandHandler = CommandHandler();
  
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
    await commandHandler.handleCommand(sessionId, commandId, payload, this);
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

  void sendToMaster(int sessionId, List<int> payload, {int packetType = dataPacket, int commandId = 0x00}) async {
    await _masterConnLock.synchronized(() async {
      if (_masterConn == null || currentState != ProxyState.connected) {
        print('Attempted to send data, but connection is not active.');
        return;
      }
      final packet = [
        packetType,                               // Packet Type (0x00 for data, 0x01 for command)
        ...ByteUtils.intToBytes(sessionId),       // Session ID
        commandId,                                // Command ID (default to 0x00 for data packets)
        ...ByteUtils.intToBytes(payload.length),  // Payload Length
        ...payload                                // Payload data
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

  // Data packet example:
  void sendDataPacket(int sessionId, List<int> data) {
    sendToMaster(sessionId, data);
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
