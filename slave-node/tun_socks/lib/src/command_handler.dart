import 'dart:convert';
import 'dart:async';
import 'package:http/http.dart' as http;

import 'protocol.dart';
import 'tun_socks_impl.dart';
import 'socks5_session.dart';
import 'version.dart';

// Command ID constants
const int speedCheck = 0x01;
const int versionCheck = 0x02;
const int heartbeatCheck = 0x03;
const int urlCheck = 0x04;
const int initSession = 0x05;

class CommandHandler {
  /// Handles incoming commands from the master based on `commandId`.
  Future<void> handleCommand(
    int sessionId,
    int? commandId,
    List<int> payload,
    TunSocks tunSocks,
  ) async {
    switch (commandId) {
      case speedCheck:
        await _performSpeedTest(payload, tunSocks);
        break;
      case versionCheck:
        _sendVersionInfo(tunSocks);
        break;
      case heartbeatCheck:
        _sendAliveResponse(tunSocks);
        break;
      case urlCheck:
        await _performUrlCheck(payload, tunSocks);
        break;
      case initSession:
        _handleInitSession(sessionId, payload, tunSocks);
        break;
      default:
        print('Unknown command received: Command ID $commandId with session ID $sessionId');
    }
  }
  
  Future<void> _performSpeedTest(List<int> payload, TunSocks tunSocks) async {
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
        final responsePayload = utf8.encode('$speedMbps');
        tunSocks.sendToMaster(0, responsePayload, packetType: commandPacket, commandId: speedCheck);
      } else {
        print('Failed to test URL: ${response.statusCode}');
      }
    } catch (e) {
      print('Error during speed test: $e');
    }
  }

  Future<void> _performUrlCheck(List<int> payload, TunSocks tunSocks) async {
    final targetUrl = utf8.decode(payload);
    try {
      final response = await http.get(Uri.parse(targetUrl));
      if (response.statusCode == 200) {
        final responsePayload = utf8.encode(response.body);
        tunSocks.sendToMaster(0, responsePayload, packetType: commandPacket, commandId: urlCheck);
      } else {
        print('Failed to fetch location data: ${response.statusCode}');
      }
    } catch (e) {
      print('Error fetching location data: $e');
    }
  }

  void _sendVersionInfo(TunSocks tunSocks) {
    final payload = utf8.encode(slaveVersion);
    tunSocks.sendToMaster(0, payload, packetType: commandPacket, commandId: versionCheck);
  }

  void _sendAliveResponse(TunSocks tunSocks) {
    tunSocks.sendToMaster(0, utf8.encode("ALIVE"), packetType: commandPacket, commandId: heartbeatCheck);
  }

  Future<void> _handleInitSession(int sessionId, List<int> payload, TunSocks tunSocks) async {
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

    final session = Socks5Session(sessionId, tunSocks.sendDataPacket);
    sessions[sessionId] = session;
    await session.initialize(address, port);
  }
}
