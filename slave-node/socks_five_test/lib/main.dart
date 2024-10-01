import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'dart:io';
import 'socks5_server.dart';

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
  Socket? _primarySocket;
  Socket? _secondarySocket;
  bool _isConnected = false;
  bool _isConnecting = false;
  int _retryAttempts = 0;
  String _connectionStatus = "";

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
    if (_retryAttempts < 5) {
      Future.delayed(Duration(seconds: 5), () async {
        _retryAttempts++;
        print('Retrying connection... Attempt $_retryAttempts');
        await _startProxyServer();
      });
    } else {
      print('Failed to reconnect after $_retryAttempts attempts');
      setState(() {
        _isConnecting = false;
      });
    }

    return Future.value();
  }

  Future<void> _startProxyServer() async {
    setState(() {
      _isConnecting = true;
      _connectionStatus = "Connecting...";
    });

    final host = '188.245.104.81';
    // final host = '192.168.12.244';

    final port = 8000;
    
    final dataport = 8001;

    try {
      _primarySocket = await Socket.connect(host, port);
      print('Connected to primary socket: $host:$port');

      setState(() {
        _isConnected = true;
        _isConnecting = false;
        _connectionStatus = "Establishsed primary connection";
      });

      _primarySocket!.listen(
        (data) {
          _handlePrimaryConnectionData(data, host, port, dataport);
        },
        onDone: () {
          print('Primary connection closed');
          setState(() {
            _isConnected = false;
            _isConnecting = false;
            _connectionStatus = "Connectin closed, trying to reconnect...";
          });
          _retryConnection();
        },
        onError: (error) {
          print('Primary connection error: $error');
          _primarySocket?.close();
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

  void _handlePrimaryConnectionData(List<int> data, String host, int port, int dataport) async {
    if (data.isNotEmpty) {
      final receivedByte = data[0];

      if (receivedByte == 56 && !_inCheckingSpeedStage) {
        _checkTimer = Stopwatch()..start();
        _inCheckingSpeedStage = true;
        _recvBytes = 0;
      }

      if (_inCheckingSpeedStage == true) {
        await _performSpeedTest(data);
      } else {
        if (receivedByte == 55) {
          print('Received byte 55, initiating secondary connection');
          _startSecondaryConnection(host, dataport);
        }
      }
    }
  }

  // Perform the speed test
  Future<void> _performSpeedTest(List<int> data) async {
    _recvBytes += data.length;

    if (_recvBytes >= _expectedDataSize && _checkTimer != null) {
      final elapsedTime = _checkTimer!.elapsedMilliseconds / 1000; // Time in seconds
      _checkTimer!.stop();
      
      print('Download completed. Total time: $elapsedTime seconds');

      // Send the elapsed time back to the server
      _sendSpeedAcknowledgment(elapsedTime);

      _inCheckingSpeedStage = false;
      _checkTimer = null;
    }
  }

  // Send speed acknowledgment to the server
  void _sendSpeedAcknowledgment(double elapsedTime) {
    if (_primarySocket != null) {
      String timeMessage = elapsedTime.toStringAsFixed(2);
      _primarySocket!.write(timeMessage);  // Send the time taken to the server
      _inCheckingSpeedStage = false;
      print('Sent time acknowledgment to server: $timeMessage');
    }
  }

  void _startSecondaryConnection(String host, int port) async {
    try {
      _secondarySocket = await Socket.connect(host, port);
      print('Connected to $host:$port (Secondary Connection)');
      setState(() {
        _connectionStatus = "Establishsed data connection";
      });
      
      Socks5ServerHandler obj = Socks5ServerHandler(_secondarySocket!);
      obj.start();
    } catch (e) {
      print('Failed to connect to secondary channel: $e');
      setState(() {
        _connectionStatus = "Failed to establish secondary connection";
      });
    }
  }

  // Stop proxy server and close connection
  void _stopProxyServer() {
    if (_primarySocket != null) {
      _primarySocket!.close();
      setState(() {
        _isConnected = false;
        _connectionStatus = "Disconnected";
      });
      print('Proxy server stopped');
    }
  }

  @override
  void dispose() {
    _primarySocket?.close();
    _secondarySocket?.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('Socket Proxy Example'),
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
