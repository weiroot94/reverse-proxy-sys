import 'dart:io';
import 'package:logging/logging.dart';
import 'package:http/http.dart' as http;
import 'package:synchronized/synchronized.dart';
import 'dart:async';
import 'dart:typed_data';

import 'socks5_session.dart';

enum ProxyState {
  disconnected,
  connecting,
  connected,
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
  bool speedTestDone = false;

  final Function(String) logCallback;
  final Function(bool) connectionStatusCallback;

  TunSocks({
    required this.host,
    required this.port,
    required this.logCallback,
    required this.connectionStatusCallback,
  });

  Future<void> startTunnel() async {
    if (currentState == ProxyState.connecting || currentState == ProxyState.connected) {
      return;
    }

    currentState = ProxyState.connecting;
    isManuallyStopped = false;
  
    logCallback('Connecting to master node...');
    connectionStatusCallback(false);

    try {
      _masterConn = await Socket.connect(host, port);
      logCallback('Connected to master node: $host:$port');

      currentState = ProxyState.connected;
      connectionStatusCallback(true);

      _masterConn!.listen(
        (data) {
          _processData(data);
        },
        onDone: () {
          logCallback('Master connection closed');
          _handleDisconnection();
        },
        onError: (error) {
          logCallback('Master connection error: $error');
          _handleDisconnection();
        },
      );
    } catch (e) {
      logCallback('Failed to connect to master: $e');
      _handleDisconnection();
    }
  }

  void _handleDisconnection() {
    currentState = ProxyState.disconnected;
    connectionStatusCallback(false);
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
    logCallback('Retrying connection... Attempt $retryAttempts');
    startTunnel();
  }

  void _processData(List<int> data) async {
    try {
        // Process data based on received commands
        if (!speedTestDone) {
            String command = String.fromCharCodes(data).trim();
            if (command.startsWith('SPEED_TEST')) {
                String url = command.split(' ')[1];
                double speedMbps = await _performSpeedTest(url);

                logCallback('Speed test result: $speedMbps Mbps');

                // Send result back to the master
                Uint8List result = Uint8List.fromList('SPEED $speedMbps\n'.codeUnits);
                _masterConn?.add(result);
                await _masterConn?.flush();

                speedTestDone = true;

                return;
            }
        }

        accumulatedBuffer.addAll(data);

        // Parse master-slave protocol
        // Frame Structure:
        // Session ID (4 bytes)
        // Payload Length (4 bytes)
        // Payload Data
        while (accumulatedBuffer.length >= 8) {
            // Check if we have at least the header size (8 bytes)
            // Header: 4 bytes for session ID + 4 bytes for payload length
            int sessionId = _bytesToInt(accumulatedBuffer.sublist(0, 4));
            int payloadLength = _bytesToInt(accumulatedBuffer.sublist(4, 8));

            // Check if the entire payload is available
            if (accumulatedBuffer.length < 8 + payloadLength) {
                // Not enough data for the entire frame, wait for more data
                break;
            }

            // Extract the payload and remove it from the buffer
            List<int> payload = accumulatedBuffer.sublist(8, 8 + payloadLength);
            accumulatedBuffer.removeRange(0, 8 + payloadLength);

            // Handle the payload for the session
            Socks5Session session = sessions.putIfAbsent(sessionId, () {
                return Socks5Session(sessionId, _masterConn, _sendToMasterInProtocol);
            });

            // Process the payload within the session
            session.processSocks5Data(payload);
        }
    } catch (e) {
      logCallback('Error processing data: $e');
      _cleanupConnections();
    }
  }

  Future<double> _performSpeedTest(String url) async {
    final stopwatch = Stopwatch()..start();

    try {
      final response = await http.get(Uri.parse(url));
      stopwatch.stop();

      if (response.statusCode == 200) {
        final contentLength = response.contentLength ?? 0;
        final elapsedTime = stopwatch.elapsedMilliseconds / 1000; // in seconds

        // Calculate speed in Mbps
        final speedMbps = (contentLength * 8) / (elapsedTime * 1000000); 
        return speedMbps;
      } else {
        print('Failed to test url: ${response.statusCode}');
        return 0.0;
      }
    } catch (e) {
      print('Error during speed test: $e');
      return 0.0;
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

  void _sendToMasterInProtocol(int sessionId, List<int> data) async {
    await _masterConnLock.synchronized(() async {
      if (_masterConn != null) {
        List<int> header = _intToBytes(sessionId) + _intToBytes(data.length);
        List<int> packet = header + data;
        
        try {
          _masterConn?.add(packet);
          await _masterConn?.flush();
        } catch (e) {
          print('Failed to send to master in protocol: $e');
        }
      }
    });
  }

  void stopTunnel() {
    isManuallyStopped = true;
    _cleanupConnections();
    connectionStatusCallback(false);
    logCallback('Proxy server stopped.');
  }

  void _cleanupConnections() {
    _masterConnLock.synchronized(() async {
      _masterConn?.close();
    });
    _masterConn?.close();
    _masterConn = null;
    speedTestDone = false;
    currentState = ProxyState.disconnected;
  }
}
