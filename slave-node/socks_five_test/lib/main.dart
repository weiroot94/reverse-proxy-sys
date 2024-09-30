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
  double? internetSpeed;
  bool _isConnected = false;
  bool _isConnecting = false;
  int _retryAttempts = 0;
  String _connectionStatus = "";

  @override
  void initState() {
    super.initState();
    _getInternetSpeed();
  }

  Future<void> _getInternetSpeed() async {
    setState(() {
      internetSpeed = null;  // To show loading state
    });

    try {
      final url = Uri.parse('https://speed.cloudflare.com/__down?bytes=5000000');  // For speed testing
      final stopwatch = Stopwatch()..start();
      final response = await http.get(url);
      stopwatch.stop();

      if (response.statusCode == 200) {
        final elapsedTime = stopwatch.elapsedMilliseconds / 1000; // Time in seconds
        final speedMbps = (response.contentLength! * 8 / 1000000) / elapsedTime;
        setState(() {
          internetSpeed = speedMbps;
          _sendSpeedToMaster();
        });
      } else {
        setState(() {
          internetSpeed = 0.0;
        });
      }
    } catch (e) {
      setState(() {
        internetSpeed = 0.0;
      });
    }
  }

  // Function to send internet speed to master
  void _sendSpeedToMaster() async {
    if (_primarySocket  != null && internetSpeed != null) {
      print('_sendSpeedToMaster function start');
      String speedMessage = '${internetSpeed?.toStringAsFixed(2)}';
      _primarySocket !.write(speedMessage);  // Sending speed data to master
      print('Sent speed to master: $speedMessage');
    }
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

      _sendSpeedToMaster();

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

  void _handlePrimaryConnectionData(List<int> data, String host, int port, int dataport) {
    if (data.isNotEmpty) {
      final receivedByte = data[0];
      print('Received data from primary connection: $receivedByte');
      if (receivedByte == 55) {
        print('Received byte 55, initiating secondary connection');
        _startSecondaryConnection(host, dataport);
      }
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
            SizedBox(height: 20),
            internetSpeed == null
                ? CircularProgressIndicator() // Show loading indicator while fetching speed
                : Text(
                    'Speed: ${internetSpeed?.toStringAsFixed(2)}Mbps',
                    style: TextStyle(fontSize: 16),
                  ),
          ],
        ),
      ),
    );
  }
}
