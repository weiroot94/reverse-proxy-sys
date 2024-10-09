import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'dart:io';
import 'dart:typed_data';
import 'socks5_session.dart';

void main() {
  runApp(MyApp());
}

class MyApp extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: MyHomePage(),
    );
  }
}

class MyHomePage extends StatefulWidget {
  @override
  _MyHomePageState createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  Socket? _master_conn;
  Socket? _dest_conn;
  bool _isConnected = false;
  bool _isConnecting = false;
  bool _isManualyStopped = false;
  int _retryAttempts = 0;
  String _connectionStatus = "";

  States currentState = States.handshake;

  // Perform Internet speed test
  bool _inCheckingSpeedStage = false;
  final int _expectedDataSize = 5000000;
  int _recvBytes = 0;
  Stopwatch? _checkTimer;

  @override
  void initState() {
    super.initState();
  }

  // Retry logic to reconnect when connection is broken
  Future<void> _retryConnection() {
    // Retry logic to reconnect when connection is broken
    if (_retryAttempts % 5 != 0 || _retryAttempts == 0) {
      // Retry the connection every 5 seconds
      Future.delayed(Duration(seconds: 5), () async {
        _retryAttempts++;
        print('Retrying connection... Attempt $_retryAttempts');
        await _startProxyServer();
      });
    } else {
      // After every 5 attempts, wait for 1 minute
      print('Reached 5 attempts. Waiting for 1 minute before retrying.');
      Future.delayed(Duration(minutes: 1), () async {
        _retryAttempts++;
        print('Resuming retries after 1 minute. Attempt $_retryAttempts');
        await _startProxyServer();
      });
    }

    return Future.value();
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
        print('Failed to download file: ${response.statusCode}');
        return 0.0;
      }
    } catch (e) {
      print('Error during speed test: $e');
      return 0.0;
    }
  } 

  Future<void> _startProxyServer() async {
    setState(() {
      _isConnecting = true;
      _isManualyStopped = false;
      _connectionStatus = "Connecting...";
    });

    final host = '188.245.104.81';
    // final host = '192.168.12.244';

    final port = 8000;
    
    try {
      _master_conn = await Socket.connect(host, port);
      print('Connected to master node: $host:$port');

      setState(() {
        _isConnected = true;
        _isConnecting = false;
        _connectionStatus = "Connected to master node, waiting for client requests...";
      });

      _master_conn!.listen(
        (data) {
          _processData(data);
        },
        onDone: () {
          print('Master connection closed');
          setState(() {
            _isConnected = false;
            _isConnecting = false;
            _connectionStatus = "Connection closed";
          });

          if (!_isManualyStopped)
            _retryConnection();
        },
        onError: (error) {
          print('Master connection error: $error');
          _master_conn?.close();
          _dest_conn?.destroy();
          setState(() {
            _isConnected = false;
            _isConnecting = false;
            _connectionStatus = "Connection error, trying to reconnect...";
          });
          _retryConnection();
        },
      );
    } catch (e) {
      print('Failed to connect to primary connection: $e');
      setState(() {
        _isConnecting = false;
        _connectionStatus = "Failed to connect, trying to reconnect...";
      });
      _retryConnection();
    }
    return Future.value();
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

  void _sendToMasterInProtocol(int sessionId, List<int> data) {
    List<int> header = _intToBytes(sessionId) + _intToBytes(data.length);
    List<int> packet = header + data;

    print('sessionId ${sessionId} SEND : ${data.length} bytes');

    _master_conn?.add(packet);
  }

  void _processData(List<int> data) async {
    try {
        String command = String.fromCharCodes(data).trim();

        if (command.startsWith('SPEED_TEST')) {
          // Master sends SPEED_TEST command with the URL
          String url = command.split(' ')[1];
          print('Received speed test command. Testing URL: $url');

          // Perform speed test
          double speedMbps = await _performSpeedTest(url);
          print('Speed test result: $speedMbps Mbps');

          // Send result back to the master
          Uint8List result = Uint8List.fromList('SPEED $speedMbps\n'.codeUnits);
          _master_conn?.add(result);
          await _master_conn?.flush();
          return;
        }

        // Parse master-slave protocol
        // Frame Structure:
        // Session ID (4 bytes)
        // Payload Length (4 bytes)
        // Payload Data
        int offset = 0;
        while (offset < data.length) {
          print('Received ${data.length} bytes from master');
          // Parse the session ID and payload length
          if (data.length - offset < 8) {
            // Not enough data for header
            break;
          }

          int sessionId = _bytesToInt(data.sublist(offset, offset + 4));
          int payloadLength = _bytesToInt(data.sublist(offset + 4, offset + 8));
          offset += 8;

          // Check if the entire payload is available
          if (data.length - offset < payloadLength) {
            // Not enough data for payload
            print('sessionId ${sessionId} ERR PAYLOAD : ${payloadLength} bytes');
            break;
          }

          // Extract payload
          List<int> payload = data.sublist(offset, offset + payloadLength);
          offset += payloadLength;

          print('sessionId ${sessionId} RECV: ${payloadLength} bytes');

          // Handle the payload for the session
          Socks5Session session;
          if (sessions.containsKey(sessionId)) {
            session = sessions[sessionId]!;
          } else {
            // Create a new session
            session = Socks5Session(sessionId, _master_conn, _sendToMasterInProtocol);
            sessions[sessionId] = session;
          }

          // Process the payload within the session
          await session.processSocks5Data(payload);
        }
    } catch (e) {
      print('Error processing data: $e');
      _master_conn?.close();
    }
  }

  // Stop proxy server and close connection
  void _stopProxyServer() {
    if (_master_conn != null) {
      _master_conn!.close();
      setState(() {
        _isConnected = false;
        _isManualyStopped = true;
        _connectionStatus = "Disconnected";
      });
      print('Proxy server stopped');
    }
  }

  @override
  void dispose() {
    _master_conn?.destroy();
    _dest_conn?.destroy();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('SOCKS5 Reverse'),
      ),
      body: Center(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            // Connection status bar
            Padding(
              padding: const EdgeInsets.all(16.0),
              child: Text(
                '$_connectionStatus',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
              ),
            ),
            // Stop Proxy Button
            ElevatedButton(
              onPressed: _isConnected ? _stopProxyServer : null,
              child: Text('Stop Proxy'),
            ),
            SizedBox(height: 20),
            // Start Proxy Button
            FloatingActionButton(
              onPressed: (!_isConnected && !_isConnecting) ? _startProxyServer : null,
              tooltip: 'Start Proxy',
              child: Icon(Icons.play_arrow),
            ),
          ],
        ),
      ),
    );
  }
}
