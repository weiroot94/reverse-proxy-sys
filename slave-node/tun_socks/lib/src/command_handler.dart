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
    try {
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
          await _handleInitSession(sessionId, payload, tunSocks);
          break;
        default:
          print('Unknown command received: Command ID $commandId with session ID $sessionId');
      }
    } catch (e, stackTrace) {
      print('Error while handling command ID $commandId for session $sessionId: $e');
      print(stackTrace);
    }
  }
  
  /// Performs a speed test using a given URL.
  Future<void> _performSpeedTest(List<int> payload, TunSocks tunSocks) async {
    final url = _decodePayload(payload, 'speedTest');
    if (url == null) return;

    final stopwatch = Stopwatch()..start();

    try {
      final response = await http.get(Uri.parse(url));
      stopwatch.stop();

      if (response.statusCode == 200) {
        final contentLength = response.contentLength ?? 0;
        final elapsedTime = stopwatch.elapsedMilliseconds / 1000; // in seconds
        final speedMbps = (contentLength * 8) / (elapsedTime * 1000000);

        print('Speed test completed: $speedMbps Mbps');
        final responsePayload = utf8.encode(speedMbps.toStringAsFixed(2));
        tunSocks.sendToMaster(0, responsePayload, packetType: commandPacket, commandId: speedCheck);
      } else {
        print('Speed test failed with status code: ${response.statusCode}');
      }
    } catch (e) {
      print('Error during speed test for URL $url: $e');
    }
  }

  /// Performs a URL check (e.g., fetch content from a URL).
  Future<void> _performUrlCheck(List<int> payload, TunSocks tunSocks) async {
    final targetUrl = _decodePayload(payload, 'urlCheck');
    if (targetUrl == null) return;

    try {
      final response = await http.get(Uri.parse(targetUrl));
      if (response.statusCode == 200) {
        print('URL check succeeded for $targetUrl');
        final responsePayload = utf8.encode(response.body);
        tunSocks.sendToMaster(0, responsePayload, packetType: commandPacket, commandId: urlCheck);
      } else {
        print('URL check failed for $targetUrl with status code: ${response.statusCode}');
      }
    } catch (e) {
      print('Error during URL check for $targetUrl: $e');
    }
  }

  /// Sends the version info to the master.
  void _sendVersionInfo(TunSocks tunSocks) {
    final payload = utf8.encode(slaveVersion);
    tunSocks.sendToMaster(0, payload, packetType: commandPacket, commandId: versionCheck);
    print('Sent version info: $slaveVersion');
  }

 /// Sends an "alive" response to the master.
  void _sendAliveResponse(TunSocks tunSocks) {
    tunSocks.sendToMaster(0, utf8.encode("ALIVE"), packetType: commandPacket, commandId: heartbeatCheck);
  }

  /// Initializes a SOCKS5 session based on the payload.
  Future<void> _handleInitSession(int sessionId, List<int> payload, TunSocks tunSocks) async {
    if (sessions.containsKey(sessionId)) {
      print('Session $sessionId already exists.');
      return;
    }

    final destinationInfo = _decodePayload(payload, 'initSession');
    if (destinationInfo == null) return;

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

    try {
      await session.initialize(address, port);
    } catch (e) {
      print('Failed to initialize session $sessionId: $e');
      sessions.remove(sessionId);
    }
  }

  /// Decodes the payload into a string and handles errors.
  String? _decodePayload(List<int> payload, String commandName) {
    try {
      final decoded = utf8.decode(payload);
      return decoded;
    } catch (e) {
      print('Failed to decode payload for $commandName: $e');
      return null;
    }
  }
}
